// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What the user chose: `<state_dir>/settings.toml`, `language = "it"` (the same small file
//! companion-sim writes), `volume = 40` (percent, set with the volume keys) and
//! `[casa_home_button] edge = "right", offset = 720` (where the user dragged the home button
//! of "Casa": an edge and the offset along it, in the view's logical pixels),
//! `ha_url = "http://ha.example:8123"` (Home Assistant picked on the first-start page
//! when the configuration has no `[home_assistant] url`), `[assistant]` (the AI, speech-to-text
//! and voice chosen on the Home Assistant page), `wake_sensitivity = "more"` (how readily the
//! character answers to his wake word: less, normal, more), `conversation_mode = true` (he
//! listens again after every spoken answer, not only after a question of his) and
//! `conversation_max = 3` (how many times in a row).

use std::path::{Path, PathBuf};

/// Settings → how readily the character answers to his wake word: a step around the cutoff the
/// detector was made with (the configuration's `wake_word.cutoff`, else the model's manifest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WakeSensitivity {
    /// Fewer false wakes (a loud room, a television): the cutoff 0.02 higher.
    Less,
    #[default]
    Normal,
    /// For an accent or a soft voice: the cutoff 0.04 lower. A Dutch "Oké Alfred" scored
    /// 0.92-0.96 against the manifest's 0.96 (near misses in the log, on one device).
    More,
}

impl WakeSensitivity {
    /// The value in settings.toml.
    pub fn key(self) -> &'static str {
        match self {
            WakeSensitivity::Less => "less",
            WakeSensitivity::Normal => "normal",
            WakeSensitivity::More => "more",
        }
    }

    pub fn from_key(key: &str) -> Option<WakeSensitivity> {
        match key {
            "less" => Some(WakeSensitivity::Less),
            "normal" => Some(WakeSensitivity::Normal),
            "more" => Some(WakeSensitivity::More),
            _ => None,
        }
    }

    /// -1, 0, 1: the screen's stepper.
    pub fn step(self) -> i32 {
        match self {
            WakeSensitivity::Less => -1,
            WakeSensitivity::Normal => 0,
            WakeSensitivity::More => 1,
        }
    }

    /// From the stepper's value, clamped.
    pub fn from_step(step: i32) -> WakeSensitivity {
        match step {
            i32::MIN..=-1 => WakeSensitivity::Less,
            0 => WakeSensitivity::Normal,
            _ => WakeSensitivity::More,
        }
    }

    /// The cutoff for a detector made with `base`. Raising never goes past 0.99 (at 1.0 nothing
    /// is ever heard) unless `base` already is.
    pub fn cutoff(self, base: f32) -> f32 {
        match self {
            WakeSensitivity::Less => (base + 0.02).min(0.99).max(base),
            WakeSensitivity::Normal => base,
            WakeSensitivity::More => (base - 0.04).max(0.0),
        }
    }
}

/// An entity id of one of `domains`: `domain.object_id`, lower case letters, digits, `_`.
pub fn valid_entity(id: &str, domains: &[&str]) -> bool {
    let Some((d, o)) = id.split_once('.') else {
        return false;
    };
    domains.contains(&d)
        && !o.is_empty()
        && o.len() <= 128
        && o.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A widget page's name as given from the browser: one line, cleaned, at most
/// [`crate::ui::widgets::MAX_PAGE_NAME`] characters ("" none).
pub fn clean_page_name(name: &str) -> String {
    crate::feeds::clean_name(name)
        .chars()
        .take(crate::ui::widgets::MAX_PAGE_NAME)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Conversation mode's windows in a row: the stepper's ends and where it starts.
pub const CONVERSATION_MAX_MIN: u32 = 1;
pub const CONVERSATION_MAX_MAX: u32 = 10;
pub const CONVERSATION_MAX_DEFAULT: u32 = 3;

#[derive(Debug, Clone)]
pub struct Settings {
    pub path: PathBuf,
}

impl Settings {
    pub fn in_dir(state_dir: &Path) -> Settings {
        Settings {
            path: state_dir.join("settings.toml"),
        }
    }

    pub fn language(&self) -> Option<String> {
        self.table()?
            .get("language")?
            .as_str()
            .filter(|l| !l.is_empty())
            .map(String::from)
    }

    /// Home Assistant's address picked on the first-start page (the configuration's wins).
    pub fn ha_url(&self) -> Option<String> {
        self.table()?
            .get("ha_url")?
            .as_str()
            .filter(|u| !u.is_empty())
            .map(String::from)
    }

    pub fn save_ha_url(&self, url: &str) -> std::io::Result<()> {
        self.save("ha_url", url.into())?;
        log::info!(event = "ha_url", url = url; "Home Assistant {url} saved in {}", self.path.display());
        Ok(())
    }

    fn table(&self) -> Option<toml::Table> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        toml::from_str(&text)
            .map_err(|e| log::warn!("{}: {e}", self.path.display()))
            .ok()
    }

    /// The saved volume in percent (0-100).
    pub fn volume(&self) -> Option<u8> {
        let v = self.table()?.get("volume")?.as_integer()?;
        u8::try_from(v).ok().filter(|v| *v <= 100)
    }

    pub fn save_volume(&self, percent: u8) {
        if let Err(e) = self.save("volume", (percent as i64).into()) {
            log::warn!("{}: {e}", self.path.display());
        }
    }

    /// Where the user left "Casa"'s home button: (edge, offset), unchecked.
    pub fn casa_home_button(&self) -> Option<(String, f32)> {
        let t = self.table()?;
        let b = t.get("casa_home_button")?.as_table()?;
        let edge = b.get("edge")?.as_str()?.to_string();
        let offset = b
            .get("offset")
            .and_then(|o| o.as_float().or_else(|| o.as_integer().map(|i| i as f64)))?;
        Some((edge, offset as f32))
    }

    pub fn save_casa_home_button(&self, edge: &str, offset: f32) {
        let mut b = toml::Table::new();
        b.insert("edge".into(), edge.into());
        b.insert("offset".into(), (offset.round() as i64).into());
        if let Err(e) = self.save("casa_home_button", b.into()) {
            log::warn!("{}: {e}", self.path.display());
        }
    }

    /// Sets one key, keeping the others (written to a temporary file, then renamed).
    /// Night mode (Settings → Screen): on, start hour, end hour; by default 23 to 7.
    pub fn night(&self) -> (bool, u32, u32) {
        let t = self
            .table()
            .and_then(|t| t.get("night")?.as_table().cloned());
        let get = |k: &str, d: i64| {
            t.as_ref()
                .and_then(|t| t.get(k)?.as_integer())
                .unwrap_or(d)
                .rem_euclid(24) as u32
        };
        let on = t
            .as_ref()
            .and_then(|t| t.get("on")?.as_bool())
            .unwrap_or(true);
        (on, get("start", 23), get("end", 7))
    }

    pub fn save_night(&self, on: bool, start: u32, end: u32) {
        let mut n = toml::Table::new();
        n.insert("on".into(), on.into());
        n.insert("start".into(), (start as i64).into());
        n.insert("end".into(), (end as i64).into());
        self.save_logged("night", n.into());
    }

    /// Settings → "Salva le conversazioni" (history.rs); on by default.
    pub fn keep_conversations(&self) -> bool {
        self.flag("keep_conversations").unwrap_or(true)
    }

    pub fn save_keep_conversations(&self, keep: bool) {
        self.save_logged("keep_conversations", keep.into());
    }

    /// Settings → Screen → "Home compatta": the clock higher and four cards; off by default.
    pub fn home_compact(&self) -> bool {
        self.flag("home_compact").unwrap_or(false)
    }

    pub fn save_home_compact(&self, on: bool) {
        self.save_logged("home_compact", on.into());
    }

    /// Settings → "Conversation mode": after every spoken answer the microphone opens again for
    /// a few seconds without the wake word (core.rs, `[conversation]` of the configuration);
    /// off by default.
    pub fn conversation_mode(&self) -> bool {
        self.flag("conversation_mode").unwrap_or(false)
    }

    pub fn save_conversation_mode(&self, on: bool) {
        self.save_logged("conversation_mode", on.into());
    }

    /// Settings → how many times in a row conversation mode listens again before the wake word
    /// is needed (a television must not keep a conversation going): 1 to 10, 3 by default.
    pub fn conversation_max(&self) -> u32 {
        self.table()
            .and_then(|t| t.get("conversation_max")?.as_integer())
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| (CONVERSATION_MAX_MIN..=CONVERSATION_MAX_MAX).contains(n))
            .unwrap_or(CONVERSATION_MAX_DEFAULT)
    }

    pub fn save_conversation_max(&self, times: u32) {
        self.save_logged("conversation_max", (times as i64).into());
    }

    /// The `[widgets]` section as it is (empty without one).
    fn widgets_table(&self) -> toml::Table {
        self.table()
            .and_then(|t| t.get("widgets")?.as_table().cloned())
            .unwrap_or_default()
    }

    /// Changes some keys of `[widgets]`, keeping the others (the markets, the cities...).
    pub(crate) fn save_widgets_keys(&self, f: impl FnOnce(&mut toml::Table)) {
        let mut t = self.widgets_table();
        f(&mut t);
        self.save_logged("widgets", t.into());
    }

    /// Settings → Screen → Widgets: the order of every page in the turn (`[widgets] order`):
    /// the pages listed, unknown and repeated keys left out, then the first page of every
    /// other kind in the default order. Without it: the pages that are on in their order, then
    /// the rest. The second page of a kind ("quotes-2") is there only when listed.
    pub fn widget_order(&self) -> Vec<crate::ui::widgets::Page> {
        use crate::ui::widgets::Page;
        let t = self.widgets_table();
        let listed = |key: &str| -> Vec<Page> {
            let mut out: Vec<Page> = Vec::new();
            for p in t
                .get(key)
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().and_then(Page::from_key))
            {
                if !out.contains(&p) {
                    out.push(p);
                }
            }
            out
        };
        let mut order = listed("order");
        if order.is_empty() {
            order = listed("on");
        }
        for p in Page::all() {
            if !order.contains(&p) {
                order.push(p);
            }
        }
        order
    }

    pub fn save_widget_order(&self, order: &[crate::ui::widgets::Page]) {
        let keys: Vec<toml::Value> = order.iter().map(|k| k.key().into()).collect();
        self.save_widgets_keys(|t| {
            t.insert("order".into(), keys.into());
        });
    }

    /// Settings → Screen → Widgets: the pages of the home's widget that are on, in the turn's
    /// order, and its interval in seconds (0 = by hand only). `[widgets] on = ["next", ...]`,
    /// `every = 20`; without the section every page is on but who is home, every 20 s. A page
    /// this file has not seen yet (`seen`; without it: the first version's three) takes its
    /// default once. An interval that is not one of the choices is the default.
    pub fn widgets(&self) -> (Vec<crate::ui::widgets::Page>, u32) {
        use crate::ui::widgets::{DEFAULT_EVERY, EVERY_CHOICES, Kind, Page};
        let default_every = DEFAULT_EVERY.as_secs() as u32;
        let t = self.widgets_table();
        let order = self.widget_order();
        let keys = |key: &str| -> Option<Vec<Page>> {
            Some(
                t.get(key)?
                    .as_array()?
                    .iter()
                    .filter_map(|v| v.as_str().and_then(Page::from_key))
                    .collect(),
            )
        };
        // Without `seen`: the first version's file. One with every page off stays so (the user
        // turned the widget off: a new page must not turn it back on).
        let seen: Vec<Kind> = keys("seen")
            .map(|s| s.iter().map(|p| p.kind).collect())
            .unwrap_or_else(|| match keys("on") {
                Some(on) if on.is_empty() => Kind::ALL.to_vec(),
                _ => Kind::FIRST.to_vec(),
            });
        let on: Vec<Page> = match keys("on") {
            Some(listed) => order
                .iter()
                .copied()
                .filter(|p| {
                    listed.contains(p)
                        || (p.n == 1 && !seen.contains(&p.kind) && p.kind.default_on())
                })
                .collect(),
            None => order
                .iter()
                .copied()
                .filter(|p| p.kind.default_on())
                .collect(),
        };
        let every = t
            .get("every")
            .and_then(|v| v.as_integer())
            .and_then(|e| u32::try_from(e).ok())
            .filter(|e| EVERY_CHOICES.contains(e))
            .unwrap_or(default_every);
        (on, every)
    }

    /// The pages of `kind` in the turn (the first always).
    fn pages_of(&self, kind: crate::ui::widgets::Kind) -> Vec<crate::ui::widgets::Page> {
        self.widget_order()
            .into_iter()
            .filter(|p| p.kind == kind)
            .collect()
    }

    /// The names given to the pages (`[widgets] names = {"quotes-2" = "Crypto"}`), cleaned.
    pub fn widget_names(&self) -> Vec<(crate::ui::widgets::Page, String)> {
        use crate::ui::widgets::Page;
        let t = self.widgets_table();
        let order = self.widget_order();
        let mut out: Vec<(Page, String)> = Vec::new();
        for (k, v) in t
            .get("names")
            .and_then(|v| v.as_table())
            .into_iter()
            .flatten()
        {
            if let (Some(p), Some(name)) = (Page::from_key(k), v.as_str())
                && order.contains(&p)
            {
                let name = clean_page_name(name);
                if !name.is_empty() {
                    out.push((p, name));
                }
            }
        }
        out
    }

    /// A page's name ("" none: the kind's word).
    pub fn save_widget_name(&self, page: crate::ui::widgets::Page, name: &str) {
        let (key, name) = (page.key(), clean_page_name(name));
        self.save_widgets_keys(|t| {
            let names = t
                .entry("names")
                .or_insert_with(|| toml::Table::new().into());
            if let Some(n) = names.as_table_mut() {
                if name.is_empty() {
                    n.remove(&key);
                } else {
                    n.insert(key, name.into());
                }
            }
        });
    }

    /// Another page of a kind that repeats: on, in the turn after the last page that is on (at
    /// `at` in the whole order when given), with an empty list.
    pub fn add_widget_page(&self, page: crate::ui::widgets::Page, at: Option<usize>, on: bool) {
        let mut order = self.widget_order();
        let (mut list, every) = self.widgets();
        if order.contains(&page) {
            return;
        }
        let at = at.unwrap_or_else(|| {
            list.last()
                .and_then(|l| order.iter().position(|p| p == l))
                .map_or(0, |i| i + 1)
        });
        order.insert(at.min(order.len()), page);
        if on {
            list.push(page);
        }
        let list: Vec<_> = order.iter().copied().filter(|p| list.contains(p)).collect();
        self.save_widget_order(&order);
        self.save_widgets(&list, every);
        let key = page.key();
        self.save_widgets_keys(|t| {
            t.remove(&key);
        });
    }

    /// A second (third...) page gone, with its list and its name.
    pub fn remove_widget_page(&self, page: crate::ui::widgets::Page) {
        if page.n <= 1 {
            return;
        }
        let order: Vec<_> = self
            .widget_order()
            .into_iter()
            .filter(|p| *p != page)
            .collect();
        let (on, every) = self.widgets();
        let on: Vec<_> = on.into_iter().filter(|p| *p != page).collect();
        self.save_widget_order(&order);
        self.save_widgets(&on, every);
        let key = page.key();
        self.save_widgets_keys(|t| {
            t.remove(&key);
            if let Some(n) = t.get_mut("names").and_then(|v| v.as_table_mut()) {
                n.remove(&key);
            }
        });
    }

    /// The markets chosen from the browser, page by page (`[[widgets.quotes]]`,
    /// `[[widgets.quotes-2]]`...): malformed entries, invalid ids and repeats left out, names
    /// cleaned, at most [`crate::feeds::MAX_QUOTES`] a page.
    pub fn quotes(&self) -> crate::ui::widgets::Lists<crate::feeds::QuoteChoice> {
        use crate::feeds::{MAX_QUOTES, QuoteChoice, clean_name};
        self.page_lists(crate::ui::widgets::Kind::Quotes, MAX_QUOTES, |v| {
            let mut q = v.clone().try_into::<QuoteChoice>().ok()?;
            q.name = clean_name(&q.name);
            q.valid().then_some((q.key(), q))
        })
    }

    pub fn save_quotes(&self, n: u8, quotes: &[crate::feeds::QuoteChoice]) {
        self.save_list(crate::ui::widgets::Kind::Quotes, n, quotes);
    }

    /// The lists of a kind's pages: `parse` gives an item and its key (repeats left out).
    fn page_lists<T: Clone>(
        &self,
        kind: crate::ui::widgets::Kind,
        max: usize,
        parse: impl Fn(&toml::Value) -> Option<(String, T)>,
    ) -> crate::ui::widgets::Lists<T> {
        let t = self.widgets_table();
        let mut out = crate::ui::widgets::Lists::default();
        for p in self.pages_of(kind) {
            let mut keys: Vec<String> = Vec::new();
            let mut list: Vec<T> = Vec::new();
            for v in t
                .get(&p.key())
                .and_then(|v| v.as_array())
                .into_iter()
                .flatten()
            {
                if let Some((k, x)) = parse(v)
                    && !keys.contains(&k)
                {
                    keys.push(k);
                    list.push(x);
                }
            }
            list.truncate(max);
            out.set(p.n, list);
        }
        out
    }

    fn save_list<T: serde::Serialize>(&self, kind: crate::ui::widgets::Kind, n: u8, items: &[T]) {
        let list: Vec<toml::Value> = items
            .iter()
            .filter_map(|q| toml::Value::try_from(q).ok())
            .collect();
        let key = crate::ui::widgets::Page::new(kind, n).key();
        self.save_widgets_keys(|t| {
            t.insert(key, list.into());
        });
    }

    /// The other cities chosen from the browser, page by page (`[[widgets.cities]]`...): as the
    /// markets, at most [`crate::feeds::MAX_CITIES`] a page, coordinates checked.
    pub fn cities(&self) -> crate::ui::widgets::Lists<crate::feeds::CityChoice> {
        use crate::feeds::{CityChoice, MAX_CITIES, clean_name};
        self.page_lists(crate::ui::widgets::Kind::Cities, MAX_CITIES, |v| {
            let mut c = v.clone().try_into::<CityChoice>().ok()?;
            c.name = clean_name(&c.name);
            c.region = clean_name(&c.region);
            c.country = clean_name(&c.country);
            (c.valid() && !c.name.is_empty()).then_some((c.key(), c))
        })
    }

    pub fn save_cities(&self, n: u8, cities: &[crate::feeds::CityChoice]) {
        self.save_list(crate::ui::widgets::Kind::Cities, n, cities);
    }

    /// Home Assistant's sensors, page by page (`[widgets] sensors = [...]`, `sensors-2`...).
    pub fn sensors(&self) -> crate::ui::widgets::Lists<String> {
        self.page_lists(
            crate::ui::widgets::Kind::Sensors,
            crate::ha::widgets::MAX_SENSORS,
            |v| {
                let id = v
                    .as_str()
                    .filter(|id| valid_entity(id, &["sensor", "input_number"]))?;
                Some((id.to_string(), id.to_string()))
            },
        )
    }

    pub fn save_sensors(&self, n: u8, ids: &[String]) {
        self.save_list(crate::ui::widgets::Kind::Sensors, n, ids);
    }

    /// Home Assistant's entities for the widget (every sensors' page's, `player`, `people`, the
    /// checks): what the hub subscribes to (ha/widgets.rs). Ids are checked for their domain.
    pub fn widget_ha(&self) -> crate::ha::widgets::Want {
        use crate::ha::widgets::MAX_PEOPLE;
        let t = self.widgets_table();
        let ids = |key: &str, domains: &[&str], max: usize| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for v in t.get(key).and_then(|v| v.as_array()).into_iter().flatten() {
                if let Some(id) = v.as_str().filter(|id| valid_entity(id, domains))
                    && !out.iter().any(|o| o == id)
                {
                    out.push(id.to_string());
                }
            }
            out.truncate(max);
            out
        };
        let flag = |k: &str| t.get(k).and_then(|v| v.as_bool()).unwrap_or(true);
        crate::ha::widgets::Want {
            sensors: self.sensors().union(|s| s.clone()),
            player: t
                .get("player")
                .and_then(|v| v.as_str())
                .filter(|id| valid_entity(id, &["media_player"]))
                .unwrap_or("")
                .to_string(),
            people: Some(ids("people", &["person"], MAX_PEOPLE)),
            openings: flag("check_openings"),
            batteries: flag("check_batteries"),
            ..Default::default()
        }
    }

    /// The player, the people and the checks (the sensors are saved page by page:
    /// [`Settings::save_sensors`]).
    pub fn save_widget_ha(&self, w: &crate::ha::widgets::Want) {
        let w = w.clone();
        self.save_widgets_keys(move |t| {
            let list = |v: &[String]| -> toml::Value {
                v.iter()
                    .map(|s| toml::Value::from(s.as_str()))
                    .collect::<Vec<_>>()
                    .into()
            };
            t.insert("player".into(), w.player.as_str().into());
            t.insert("people".into(), list(w.people.as_deref().unwrap_or(&[])));
            t.insert("check_openings".into(), w.openings.into());
            t.insert("check_batteries".into(), w.batteries.into());
        });
    }

    /// The currency of the coins (`[widgets] currency`): "usd" unless one of the choices.
    pub fn widget_currency(&self) -> String {
        self.widgets_table()
            .get("currency")
            .and_then(|v| v.as_str())
            .filter(|c| crate::feeds::coingecko::valid_currency(c))
            .unwrap_or("usd")
            .to_string()
    }

    pub fn save_widget_currency(&self, currency: &str) {
        let c = currency.to_string();
        self.save_widgets_keys(|t| {
            t.insert("currency".into(), c.into());
        });
    }

    /// The pages on and the interval; every page is then `seen` (its default never applies
    /// again).
    pub fn save_widgets(&self, on: &[crate::ui::widgets::Page], every: u32) {
        use crate::ui::widgets::Kind;
        let keys: Vec<toml::Value> = on.iter().map(|k| k.key().into()).collect();
        let seen: Vec<toml::Value> = Kind::ALL.iter().map(|k| k.key().into()).collect();
        // The places of the pages turned off are kept: the whole order is written once.
        let order: Vec<toml::Value> = self.widget_order().iter().map(|k| k.key().into()).collect();
        self.save_widgets_keys(|t| {
            t.entry("order").or_insert(order.into());
            t.insert("on".into(), keys.into());
            t.insert("every".into(), (every as i64).into());
            t.insert("seen".into(), seen.into());
        });
    }

    /// Updates (Settings → Info): offer the release candidates too; None = as the configuration
    /// says (`[update] prerelease`).
    pub fn update_prerelease(&self) -> Option<bool> {
        self.flag("update_prerelease")
    }

    pub fn save_update_prerelease(&self, on: bool) {
        self.save_logged("update_prerelease", on.into());
    }

    /// Settings → Access from the browser (the panel): `[panel] on`; off by default.
    pub fn panel_on(&self) -> bool {
        self.table()
            .and_then(|t| t.get("panel")?.as_table()?.get("on")?.as_bool())
            .unwrap_or(false)
    }

    pub fn save_panel_on(&self, on: bool) {
        self.save_panel_key("on", on);
    }

    /// Settings → Access from the browser → Remote screen: `[panel] screen`; off by default.
    pub fn panel_screen(&self) -> bool {
        self.table()
            .and_then(|t| t.get("panel")?.as_table()?.get("screen")?.as_bool())
            .unwrap_or(false)
    }

    pub fn save_panel_screen(&self, on: bool) {
        self.save_panel_key("screen", on);
    }

    /// One key of `[panel]`, the others kept.
    fn save_panel_key(&self, key: &str, on: bool) {
        let mut t = self
            .table()
            .and_then(|t| t.get("panel")?.as_table().cloned())
            .unwrap_or_default();
        t.insert(key.into(), on.into());
        self.save_logged("panel", t.into());
    }

    /// The control center's microphone switch: muted, the wake word is not listened for.
    pub fn mic_muted(&self) -> bool {
        self.flag("mic_muted").unwrap_or(false)
    }

    pub fn save_mic_muted(&self, muted: bool) {
        self.save_logged("mic_muted", muted.into());
    }

    /// The control center's AirPlay switch (when AirPlay is installed); on by default.
    pub fn airplay_on(&self) -> bool {
        self.flag("airplay").unwrap_or(true)
    }

    pub fn save_airplay_on(&self, on: bool) {
        self.save_logged("airplay", on.into());
    }

    /// The `todo.*` lists shown on the screen (Home Assistant page → Lists and calendars);
    /// empty = all of them.
    pub fn lists(&self) -> Vec<String> {
        self.string_list("lists")
    }

    pub fn save_lists(&self, lists: &[String]) {
        self.save_string_list("lists", lists);
    }

    /// The `calendar.*` entities shown on the screen; empty = all of them.
    pub fn calendars(&self) -> Vec<String> {
        self.string_list("calendars")
    }

    pub fn save_calendars(&self, calendars: &[String]) {
        self.save_string_list("calendars", calendars);
    }

    fn string_list(&self, key: &str) -> Vec<String> {
        self.table()
            .and_then(|t| t.get(key)?.as_array().cloned())
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    }

    fn save_string_list(&self, key: &str, list: &[String]) {
        let v: Vec<toml::Value> = list.iter().map(|s| s.as_str().into()).collect();
        self.save_logged(key, v.into());
    }

    /// Home Assistant page → the AI, the speech-to-text, the voice engine and its voice chosen on
    /// the screen (`[assistant] ai = <config entry>, stt = "stt.…", tts = "tts.…", voice = "<id>"`);
    /// empty = not chosen.
    pub fn assistant(&self) -> crate::ha::setup::Choice {
        let t = self
            .table()
            .and_then(|t| t.get("assistant")?.as_table().cloned())
            .unwrap_or_default();
        let get = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
        crate::ha::setup::Choice {
            ai: get("ai"),
            stt: get("stt"),
            tts: get("tts"),
            voice: get("voice"),
        }
    }

    pub fn save_assistant(&self, c: &crate::ha::setup::Choice) {
        let mut t = toml::Table::new();
        for (k, v) in [
            ("ai", &c.ai),
            ("stt", &c.stt),
            ("tts", &c.tts),
            ("voice", &c.voice),
        ] {
            if !v.is_empty() {
                t.insert(k.into(), v.as_str().into());
            }
        }
        self.save_logged("assistant", t.into());
    }

    /// "Più tardi" on the setup page: it does not come back by itself at start.
    pub fn setup_later(&self) -> bool {
        self.flag("setup_later").unwrap_or(false)
    }

    pub fn save_setup_later(&self) {
        self.save_logged("setup_later", true.into());
    }

    /// Home Assistant picked on screen after a "Più tardi": the link page comes at start again.
    pub fn clear_setup_later(&self) {
        if self.setup_later() {
            self.save_logged("setup_later", false.into());
        }
    }

    /// Settings → the wake word's sensitivity; normal by default.
    pub fn wake_sensitivity(&self) -> WakeSensitivity {
        self.table()
            .and_then(|t| {
                t.get("wake_sensitivity")?
                    .as_str()
                    .and_then(WakeSensitivity::from_key)
            })
            .unwrap_or_default()
    }

    pub fn save_wake_sensitivity(&self, s: WakeSensitivity) {
        self.save_logged("wake_sensitivity", s.key().into());
    }

    /// `board.json`, next to settings.toml (the board's pages and custom widgets: ui/board.rs).
    pub fn board_path(&self) -> PathBuf {
        self.path.with_file_name("board.json")
    }

    /// The board, validated (without Home Assistant's catalogue: the gates read the device
    /// classes again when the screen draws). Missing: empty; broken: empty, said in the log;
    /// a tile that cannot stay is left out, said in the log.
    pub fn board(&self) -> crate::ui::board::Board {
        use crate::ui::board;
        let path = self.board_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return board::Board::default(),
            Err(e) => {
                log::warn!(event = "board"; "{}: {e}: an empty board", path.display());
                return board::Board::default();
            }
        };
        let (b, mut why) = board::parse(&text);
        let (b, more) = board::validate(b, &[]);
        why.extend(more);
        for w in &why {
            log::warn!(event = "board"; "{}: {w}", path.display());
        }
        b
    }

    /// Writes the board (to a temporary file, then renamed: never half a file).
    pub fn save_board(&self, b: &crate::ui::board::Board) -> std::io::Result<()> {
        let path = self.board_path();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.new");
        std::fs::write(&tmp, crate::ui::board::to_json(b))?;
        std::fs::rename(&tmp, &path)
    }

    fn flag(&self, key: &str) -> Option<bool> {
        self.table()?.get(key)?.as_bool()
    }

    fn save_logged(&self, key: &str, value: toml::Value) {
        if let Err(e) = self.save(key, value) {
            log::warn!("{}: {e}", self.path.display());
        }
    }

    fn save(&self, key: &str, value: toml::Value) -> std::io::Result<()> {
        let mut table: toml::Table = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        table.insert(key.into(), value);
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("toml.new");
        std::fs::write(&tmp, toml::to_string(&table).unwrap_or_default())?;
        std::fs::rename(&tmp, &self.path)
    }

    pub fn save_language(&self, lang: &str) {
        let mut table: toml::Table = std::fs::read_to_string(&self.path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default();
        table.insert("language".into(), lang.into());
        let result = self
            .path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&self.path, toml::to_string(&table).unwrap_or_default()));
        match result {
            Ok(()) => {
                log::info!(event = "language", language = lang; "language {lang} saved in {}", self.path.display())
            }
            Err(e) => log::warn!("{}: {e}", self.path.display()),
        }
    }
}

/// What the first start still has to ask, after [`apply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FirstStart {
    /// No language in the configuration nor in the settings: the welcome page.
    pub language: bool,
    /// No Home Assistant address in either: the page that finds it on the network.
    pub home_assistant: bool,
}

/// The language and Home Assistant's address in use: the language chosen on screen wins over
/// the configuration's (Settings → Language), the configuration's address wins over the one
/// picked on screen (the installer's `--ha-url` or a hand edit is deliberate). `None` / "" =
/// not chosen yet.
pub fn resolve(
    cfg_language: &str,
    saved_language: Option<String>,
    cfg_url: &str,
    saved_url: Option<String>,
) -> (Option<String>, String) {
    let language = saved_language
        .filter(|l| !l.is_empty())
        .or_else(|| (!cfg_language.is_empty()).then(|| cfg_language.to_string()));
    let url = if cfg_url.is_empty() {
        saved_url.unwrap_or_default()
    } else {
        cfg_url.to_string()
    };
    (language, url)
}

/// Puts the settings' language and Home Assistant address into `cfg` (see [`resolve`]); a
/// language never chosen becomes [`locale::DEFAULT`] until the welcome page.
pub fn apply(cfg: &mut crate::config::Config, s: &Settings) -> FirstStart {
    let (language, url) = resolve(
        &cfg.character.language,
        s.language(),
        &cfg.home_assistant.url,
        s.ha_url(),
    );
    let first = FirstStart {
        language: language.is_none(),
        home_assistant: url.is_empty(),
    };
    cfg.character.language = language.unwrap_or_else(|| locale::DEFAULT.to_string());
    cfg.home_assistant.url = url;
    first
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wake_sensitivity_steps_around_the_base() {
        use WakeSensitivity::*;
        let near = |a: f32, b: f32| (a - b).abs() < 1e-6;
        assert!(near(Normal.cutoff(0.96), 0.96));
        assert!(near(More.cutoff(0.96), 0.92));
        assert!(near(Less.cutoff(0.96), 0.98));
        assert!(near(Less.cutoff(0.98), 0.99), "never past 0.99");
        assert!(near(Less.cutoff(0.995), 0.995), "a base above 0.99 stays");
        assert!(near(More.cutoff(0.02), 0.0));
        for s in [Less, Normal, More] {
            assert_eq!(WakeSensitivity::from_key(s.key()), Some(s));
            assert_eq!(WakeSensitivity::from_step(s.step()), s);
        }
        assert_eq!(WakeSensitivity::from_step(-5), Less);
        assert_eq!(WakeSensitivity::from_step(7), More);
        assert_eq!(WakeSensitivity::from_key("loud"), None);
    }

    #[test]
    fn quotes_read_tolerantly() {
        use crate::feeds::{QuoteChoice, Source};
        let dir =
            std::env::temp_dir().join(format!("home-companion-quotes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::in_dir(&dir);
        assert!(s.quotes().is_empty());
        let q = |src, id: &str| QuoteChoice {
            source: src,
            id: id.into(),
            name: format!(" {id}\n"),
        };
        s.save_quotes(
            1,
            &[q(Source::Yahoo, "AAPL"), q(Source::Coingecko, "bitcoin")],
        );
        s.save_widgets(&[crate::ui::widgets::Kind::Quotes.into()], 20);
        let back = s.quotes().get(1).to_vec();
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].name, "AAPL");
        assert_eq!(back[1].key(), "coingecko:bitcoin");
        std::fs::write(
            &s.path,
            "[widgets]\nquotes = [{source = \"yahoo\", id = \"AAPL\", name = \"A\"}, \
             {source = \"yahoo\", id = \"AAPL\", name = \"again\"}, {source = \"ftp\", id = \"x\", name = \"\"}, \
             {source = \"yahoo\", id = \"../etc\", name = \"\"}, 7]\n",
        )
        .unwrap();
        assert_eq!(s.quotes().get(1).len(), 1);
        let many: Vec<QuoteChoice> = (0..12)
            .map(|i| q(Source::Yahoo, &format!("S{i}")))
            .collect();
        s.save_quotes(1, &many);
        assert_eq!(s.quotes().get(1).len(), crate::feeds::MAX_QUOTES);
        assert_eq!(s.widget_currency(), "usd");
        s.save_widget_currency("chf");
        assert_eq!(s.widget_currency(), "chf");
        s.save_widget_currency("btc");
        assert_eq!(s.widget_currency(), "usd");
        // The cities, the same way.
        let city = |id: u64, lat: f64| crate::feeds::CityChoice {
            id,
            name: "Berlin".into(),
            region: "Berlin".into(),
            country: "DE".into(),
            lat,
            lon: 13.41,
        };
        s.save_cities(
            1,
            &[city(1, 52.52), city(1, 52.52), city(2, 95.0), city(3, -1.0)],
        );
        let back = s.cities().get(1).to_vec();
        assert_eq!(back.iter().map(|c| c.id).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(
            s.quotes().get(1).len(),
            crate::feeds::MAX_QUOTES,
            "the markets are kept"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn widgets_default_on_and_read_back() {
        use crate::ui::widgets::{Kind, Page};
        let dir =
            std::env::temp_dir().join(format!("home-companion-widgets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::in_dir(&dir);
        let defaults: Vec<Page> = Kind::ALL
            .into_iter()
            .filter(|k| *k != Kind::Home)
            .map(Page::from)
            .collect();
        assert_eq!(s.widgets(), (defaults.clone(), 20));
        assert_eq!(s.widget_order(), Page::all());
        // The order is the user's.
        s.save_widget_order(&[Kind::Shopping.into(), Kind::Next.into()]);
        assert_eq!(
            &s.widget_order()[..3],
            [Kind::Shopping, Kind::Next, Kind::Quotes]
        );
        s.save_widgets(&[Kind::Next.into(), Kind::Shopping.into()], 0);
        assert_eq!(
            s.widgets(),
            (vec![Kind::Shopping.into(), Kind::Next.into()], 0)
        );
        s.save_widgets(&[], 60);
        assert_eq!(s.widgets(), (Vec::new(), 60));
        // Other keys are kept, and the order too.
        s.save_home_compact(true);
        assert_eq!(s.widgets(), (Vec::new(), 60));
        assert_eq!(s.widget_order()[0], Kind::Shopping);
        s.save_widgets_keys(|t| {
            t.insert("currency".into(), "eur".into());
        });
        s.save_widgets(&[Kind::Home.into()], 20);
        assert!(s.table().unwrap()["widgets"].get("currency").is_some());
        assert_eq!(s.widgets(), (vec![Kind::Home.into()], 20));
        // The first version's file: its pages in its order, the new ones on at the end
        // (who is home stays off); a page turned off there stays off.
        std::fs::write(
            &s.path,
            "[widgets]\non = [\"shopping\", \"stocks\", \"next\"]\nevery = 7\n",
        )
        .unwrap();
        let (on, every) = s.widgets();
        assert_eq!(every, 20);
        assert_eq!(
            on,
            [
                Kind::Shopping,
                Kind::Next,
                Kind::Quotes,
                Kind::Cities,
                Kind::Notes,
                Kind::Sensors,
                Kind::Check,
                Kind::Playing
            ]
        );
        // A first version's file with every page off: all stay off.
        std::fs::write(&s.path, "[widgets]\non = []\nevery = 30\n").unwrap();
        assert_eq!(s.widgets(), (Vec::new(), 30));
        std::fs::write(
            &s.path,
            "[widgets]\non = [\"shopping\", \"stocks\", \"next\"]\nevery = 7\n",
        )
        .unwrap();
        // Saved once: every page seen, turning one off sticks.
        s.save_widgets(&[Kind::Next.into()], 20);
        assert_eq!(s.widgets().0, [Kind::Next]);
        assert_eq!(s.widget_order()[0], Kind::Shopping);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file of before several pages of a kind: its one list is the first page's, nothing to
    /// do; a second page has its own list, its place, its name; removed, it leaves nothing.
    #[test]
    fn pages_of_a_kind_and_the_file_of_before() {
        use crate::ui::widgets::{Kind, Page};
        let dir = std::env::temp_dir().join(format!("home-companion-pages-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::in_dir(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            &s.path,
            "[widgets]\nevery = 20\non = [\"quotes\", \"weather\"]\n\
             order = [\"next\", \"quotes\", \"weather\"]\n\
             seen = [\"next\", \"quotes\", \"weather\", \"cities\", \"shopping\", \"notes\", \"sensors\", \"check\", \"playing\", \"home\"]\n\
             sensors = [\"sensor.a\"]\n\
             [[widgets.quotes]]\nid = \"bitcoin\"\nname = \"Bitcoin\"\nsource = \"coingecko\"\n",
        )
        .unwrap();
        let q1: Page = Kind::Quotes.into();
        let q2 = Page::new(Kind::Quotes, 2);
        assert_eq!(s.widgets().0, [q1, Kind::Weather.into()]);
        assert_eq!(
            s.widget_order()[..3],
            [Kind::Next.into(), q1, Kind::Weather.into()]
        );
        assert_eq!(s.widget_order().len(), Kind::ALL.len());
        assert_eq!(s.quotes().get(1)[0].key(), "coingecko:bitcoin");
        assert_eq!(s.quotes().0.len(), 1);
        assert_eq!(s.sensors().get(1), ["sensor.a"]);
        assert_eq!(s.widget_ha().sensors, ["sensor.a"]);
        // A second markets' page: on, after the last one on, its own list.
        s.add_widget_page(q2, None, true);
        assert_eq!(s.widgets().0, [q1, Kind::Weather.into(), q2]);
        let o = s.widget_order();
        assert_eq!(o.iter().position(|p| *p == q2), Some(3));
        let q = |id: &str| crate::feeds::QuoteChoice {
            source: crate::feeds::Source::Yahoo,
            id: id.into(),
            name: id.into(),
        };
        s.save_quotes(2, &[q("AAPL"), q("GC=F")]);
        s.save_widget_name(q2, "  My\nstocks and more and more and more ");
        let names = s.widget_names();
        assert_eq!(names, [(q2, "My stocks and more and m".to_string())]);
        assert_eq!(s.quotes().get(2).len(), 2);
        assert_eq!(
            s.quotes().get(1).len(),
            1,
            "the first page's list untouched"
        );
        let text = std::fs::read_to_string(&s.path).unwrap();
        assert!(text.contains("[[widgets.quotes-2]]"), "{text}");
        // A second sensors' page: the hub asks for both pages' sensors, once.
        let s2 = Page::new(Kind::Sensors, 2);
        s.add_widget_page(s2, Some(0), false);
        s.save_sensors(2, &["sensor.b".into(), "sensor.a".into()]);
        assert_eq!(s.widget_order()[0], s2);
        assert!(!s.widgets().0.contains(&s2));
        assert_eq!(s.widget_ha().sensors, ["sensor.a", "sensor.b"]);
        // A list or a name of a page not in the turn is not read.
        s.save_widgets_keys(|t| {
            t.insert("cities-3".into(), toml::Value::Array(Vec::new()));
        });
        assert_eq!(s.cities().0.len(), 1);
        // Removed: its place, its list, its name gone; the first page is never removed.
        s.remove_widget_page(q2);
        s.remove_widget_page(q1);
        assert!(!s.widget_order().contains(&q2) && s.widget_order().contains(&q1));
        assert!(s.widget_names().is_empty());
        assert_eq!(s.quotes().0.len(), 1);
        let text = std::fs::read_to_string(&s.path).unwrap();
        assert!(!text.contains("quotes-2"), "{text}");
        assert!(text.contains("coingecko"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saves_and_reads_back() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::in_dir(&dir);
        assert_eq!(s.language(), None);
        s.save_language("it");
        assert_eq!(s.language().as_deref(), Some("it"));
        assert_eq!(s.volume(), None);
        s.save_volume(35);
        assert_eq!(s.volume(), Some(35));
        assert_eq!(s.language().as_deref(), Some("it"));
        s.save_language("en");
        assert_eq!(s.volume(), Some(35));
        assert_eq!(s.casa_home_button(), None);
        s.save_casa_home_button("left", 300.4);
        assert_eq!(s.casa_home_button(), Some(("left".into(), 300.0)));
        assert_eq!(s.volume(), Some(35));
        assert_eq!(s.language().as_deref(), Some("en"));
        s.save_volume(40);
        assert_eq!(s.casa_home_button(), Some(("left".into(), 300.0)));
        assert!(!s.home_compact(), "normal by default");
        s.save_home_compact(true);
        assert!(s.home_compact());
        assert_eq!(
            s.wake_sensitivity(),
            WakeSensitivity::Normal,
            "normal by default"
        );
        s.save_wake_sensitivity(WakeSensitivity::More);
        assert_eq!(s.wake_sensitivity(), WakeSensitivity::More);
        assert!(s.home_compact(), "the other keys stay");
        assert_eq!(s.night(), (true, 23, 7));
        s.save_night(false, 22, 6);
        assert_eq!(s.night(), (false, 22, 6));
        assert!(s.keep_conversations());
        s.save_keep_conversations(false);
        assert!(!s.keep_conversations());
        assert!(!s.conversation_mode(), "off by default");
        s.save_conversation_mode(true);
        assert!(s.conversation_mode());
        assert_eq!(s.conversation_max(), 3, "three by default");
        s.save_conversation_max(5);
        assert_eq!(s.conversation_max(), 5);
        s.save_conversation_max(0);
        assert_eq!(s.conversation_max(), 3, "out of range: the default");
        assert!(!s.keep_conversations(), "the other keys stay");
        assert!(!s.setup_later());
        s.save_setup_later();
        assert!(s.setup_later());
        assert_eq!(s.volume(), Some(40));
        std::fs::write(&s.path, "volume = 300\n").unwrap();
        assert_eq!(s.volume(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_board_is_written_whole_and_read_back() {
        use crate::ui::board::{Board, Canvas, Tile, TileKind};
        let dir = std::env::temp_dir().join(format!(
            "home-companion-settings-board-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::in_dir(&dir);
        assert_eq!(s.board_path(), dir.join("board.json"));
        assert_eq!(s.board(), Board::default(), "missing: empty");
        let b = Board {
            pages: vec![Canvas {
                id: "p1".into(),
                name: "Living room".into(),
                tiles: vec![Tile {
                    x: 0,
                    y: 0,
                    w: 2,
                    h: 1,
                    kind: TileKind::Button,
                    entity_id: "light.living_room".into(),
                    name: String::new(),
                    icon: String::new(),
                    confirm: None,
                    service: None,
                    service_data: None,
                }],
            }],
            ..Default::default()
        };
        s.save_board(&b).unwrap();
        assert_eq!(s.board(), b);
        assert!(!dir.join("board.json.new").exists(), "renamed into place");
        // A tile that cannot stay goes; the rest stays.
        let mut bad = b.clone();
        bad.pages[0].tiles.push(Tile {
            entity_id: "lock.front".into(),
            x: 2,
            ..b.pages[0].tiles[0].clone()
        });
        s.save_board(&bad).unwrap();
        assert_eq!(s.board(), b);
        std::fs::write(s.board_path(), "{ not json").unwrap();
        assert_eq!(s.board(), Board::default(), "broken: empty");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lists_and_calendars_round_trip() {
        let dir = std::env::temp_dir().join(format!(
            "home-companion-settings-lists-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::in_dir(&dir);
        assert!(s.lists().is_empty() && s.calendars().is_empty());
        s.save_lists(&["todo.shopping".into(), "todo.bring".into()]);
        s.save_calendars(&["calendar.family".into()]);
        s.save_language("it");
        assert_eq!(s.lists(), ["todo.shopping", "todo.bring"]);
        assert_eq!(s.calendars(), ["calendar.family"]);
        s.save_lists(&[]);
        assert!(s.lists().is_empty());
        assert_eq!(s.assistant(), Default::default());
        let c = crate::ha::setup::Choice {
            ai: "01ENTRY".into(),
            stt: "stt.example".into(),
            tts: String::new(),
            voice: "cedar".into(),
        };
        s.save_assistant(&c);
        assert_eq!(s.assistant(), c);
        assert_eq!(s.lists(), Vec::<String>::new());
        assert_eq!(s.calendars(), ["calendar.family"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn saved_url_and_resolution() {
        let dir = std::env::temp_dir().join(format!(
            "home-companion-settings-url-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let s = Settings::in_dir(&dir);
        assert_eq!(s.ha_url(), None);
        s.save_volume(30);
        s.save_ha_url("http://ha.example:8123").unwrap();
        assert_eq!(s.ha_url().as_deref(), Some("http://ha.example:8123"));
        assert_eq!(s.volume(), Some(30));

        let some = |v: &str| Some(v.to_string());
        // Nothing anywhere: both still to ask.
        assert_eq!(resolve("", None, "", None), (None, String::new()));
        // An old install: the configuration's language, no settings.
        assert_eq!(resolve("it", None, "", None).0, some("it"));
        // The screen's language wins over the configuration's.
        assert_eq!(resolve("it", some("en"), "", None).0, some("en"));
        assert_eq!(resolve("", some(""), "", None).0, None);
        // The configuration's address wins; the saved one fills an empty configuration.
        assert_eq!(
            resolve(
                "",
                None,
                "http://192.0.2.10:8123",
                some("http://192.0.2.11:8123")
            )
            .1,
            "http://192.0.2.10:8123"
        );
        assert_eq!(
            resolve("", None, "", some("http://192.0.2.11:8123")).1,
            "http://192.0.2.11:8123"
        );

        let mut cfg = crate::config::Config::default();
        cfg.character.language = String::new();
        cfg.home_assistant.url = String::new();
        assert_eq!(
            apply(&mut cfg, &s),
            FirstStart {
                language: true,
                home_assistant: false
            }
        );
        assert_eq!(cfg.character.language, locale::DEFAULT);
        assert_eq!(cfg.home_assistant.url, "http://ha.example:8123");
        s.save_language("it");
        let mut cfg = crate::config::Config::default();
        cfg.home_assistant.url = "http://192.0.2.10:8123".into();
        assert_eq!(
            apply(&mut cfg, &s),
            FirstStart {
                language: false,
                home_assistant: false
            }
        );
        assert_eq!(cfg.character.language, "it");
        assert_eq!(cfg.home_assistant.url, "http://192.0.2.10:8123");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
