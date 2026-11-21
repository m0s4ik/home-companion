// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A body for trying the browser panel without the device: the real protocol, sessions and
//! settings model on `panel.sock`, sample shopping lists, and a screen that answers every access
//! request by itself (Allow after `ALLOW_AFTER`, or Refuse with `--refuse`). For the panel's
//! screenshots:
//!
//! ```sh
//! cargo run -p home-companion --example panel_body -- /tmp/panel-demo &
//! cargo run -p panel -- --socket /tmp/panel-demo/panel.sock --listen 127.0.0.1:8125
//! ```
//!
//! The remote screen is on, its picture a sample home (the golden `widget.png`) with a gold ring
//! where each touch went; `touch <dir>/screen-off` switches it off as on the device, `touch
//! <dir>/screen-private` makes the screen show a private page.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use home_companion::feeds::{
    Choice, CityChoice, Found, Hit, QuoteChoice, SearchState, Source, What,
};
use home_companion::panel::protocol::{self, PanelHost};
use home_companion::panel::sessions::{self, Request, Sessions};
use home_companion::settings::Settings;
use home_companion::settings_model::{self as model, Change, Host};
use home_companion::ui::capture::{self, Frame};
use home_companion::ui::lists::{self, ShopList};
use home_companion::ui::screen::samples;
use home_companion::ui::widgets::{
    self, Chosen, CityRow, Kind, Person, Place, Sources, Trend, WidgetRow,
};
use serde_json::{Value, json};

const ALLOW_AFTER: Duration = Duration::from_secs(4);

struct Demo {
    notes: RefCell<Vec<(i64, String)>>,
    /// When a chosen item was first seen without data: its numbers "come" 3 s later.
    seen: RefCell<std::collections::HashMap<String, Instant>>,
    /// When each search started: its rows "come" a second after the names.
    started: RefCell<std::collections::HashMap<String, Instant>>,
    searches: RefCell<std::collections::HashMap<String, SearchState>>,
    settings: Settings,
    lists: RefCell<Vec<ShopList>>,
    asked: RefCell<Option<(String, Instant)>>,
    volume: RefCell<u8>,
    dir: PathBuf,
    born: Instant,
    /// The sample screen, RGB at half size (640 x 400).
    picture: Vec<u8>,
    taps: RefCell<Vec<(f32, f32, Instant)>>,
    frame: RefCell<Option<Frame>>,
}

const PIC_W: u32 = 640;
const PIC_H: u32 = 400;

/// The sample screen at half size: the golden picture of the home with the widget.
fn picture() -> Vec<u8> {
    let png = include_bytes!("../out/golden/widget.png");
    let img = image::load_from_memory(png).expect("golden png").to_rgb8();
    image::imageops::resize(&img, PIC_W, PIC_H, image::imageops::FilterType::Triangle).into_raw()
}

impl Demo {
    /// A new picture four times a second: the sample, the touches of the last second as gold
    /// rings, and a line at the bottom that moves (so that every frame differs).
    fn draw(&self) -> Frame {
        let seq = self.born.elapsed().as_millis() as u64 / 250 + 1;
        if let Some(f) = self.frame.borrow().as_ref().filter(|f| f.seq == seq) {
            return f.clone();
        }
        let mut rgb = self.picture.clone();
        let mut put = |x: i64, y: i64, c: [u8; 3]| {
            if (0..PIC_W as i64).contains(&x) && (0..PIC_H as i64).contains(&y) {
                let i = ((y * PIC_W as i64 + x) * 3) as usize;
                rgb[i..i + 3].copy_from_slice(&c);
            }
        };
        self.taps
            .borrow_mut()
            .retain(|t| t.2.elapsed() < Duration::from_millis(1200));
        for (x, y, _) in self.taps.borrow().iter() {
            let (cx, cy) = ((*x * PIC_W as f32) as i64, (*y * PIC_H as f32) as i64);
            for dy in -24..=24i64 {
                for dx in -24..=24i64 {
                    let d = ((dx * dx + dy * dy) as f64).sqrt();
                    if (18.0..=22.0).contains(&d) {
                        put(cx + dx, cy + dy, [212, 171, 99]);
                    }
                }
            }
        }
        let at = (seq % 64) as i64 * PIC_W as i64 / 64;
        for x in at..at + 24 {
            for y in PIC_H as i64 - 3..PIC_H as i64 {
                put(x, y, [212, 171, 99]);
            }
        }
        let jpeg = capture::encode_jpeg(&rgb, PIC_W, PIC_H).expect("jpeg");
        let f = Frame {
            seq,
            width: PIC_W,
            height: PIC_H,
            b64: std::sync::Arc::new(capture::base64(&jpeg)),
            jpeg: std::sync::Arc::new(jpeg),
        };
        *self.frame.borrow_mut() = Some(f.clone());
        f
    }
}

impl Host for Demo {
    fn settings(&self) -> Option<&Settings> {
        Some(&self.settings)
    }
    fn live(&self, key: &str) -> Option<Value> {
        match key {
            "volume" => Some(json!(*self.volume.borrow())),
            "language" => Some(json!(
                self.settings.language().unwrap_or_else(|| "en".into())
            )),
            _ => None,
        }
    }
    fn dynamic_options(&self, key: &str) -> Vec<(String, String)> {
        let v: &[(&str, &str)] = match key {
            "language" => &[
                ("en", "English"),
                ("it", "Italiano"),
                ("de", "Deutsch"),
                ("nl", "Nederlands"),
            ],
            "lists" => &[
                ("todo.groceries", "Groceries"),
                ("todo.hardware", "Hardware"),
            ],
            "calendars" => &[("calendar.family", "Family"), ("calendar.work", "Work")],
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
        eprintln!("applied: {change:?}");
    }
}

impl PanelHost for Demo {
    fn panel_on(&self) -> bool {
        true
    }
    fn can_ask(&self) -> bool {
        true
    }
    fn name(&self) -> String {
        "Alfred".into()
    }
    fn words(&self, lang: &str) -> Box<dyn Fn(&str) -> String + '_> {
        let l = locale::load_or_english(&locale::repo_locales(), lang).expect("locales");
        Box::new(move |k| l.text(k).replace("{name}", "Alfred"))
    }
    fn languages(&self) -> Vec<String> {
        locale::available(&locale::repo_locales())
    }
    fn ask(&self, r: &Request) {
        eprintln!(
            "screen: {} {} asks, code {}",
            r.agent.browser, r.peer, r.code
        );
        *self.asked.borrow_mut() = Some((r.id.clone(), Instant::now()));
    }
    fn shopping(&self) -> Vec<ShopList> {
        self.lists.borrow().clone()
    }
    fn shopping_add(&self, list: &str, text: &str) {
        let mut l = self.lists.borrow_mut();
        if let Some(l) = l.iter_mut().find(|l| l.entity == list) {
            let uid = format!("u{}", l.items.len() + 100);
            l.items.push((uid, text.to_string()));
        }
    }
    fn shopping_done(&self, list: &str, uid: &str) {
        lists::remove(&mut self.lists.borrow_mut(), list, uid);
    }
    fn shopping_remove(&self, list: &str, uid: &str) {
        lists::remove(&mut self.lists.borrow_mut(), list, uid);
    }
    fn sessions_changed(&self) {}
    fn widget_pages(&self) -> Vec<(home_companion::ui::widgets::Kind, bool)> {
        let on = self.settings.widgets().0;
        self.settings
            .widget_order()
            .into_iter()
            .map(|k| (k, on.contains(&k)))
            .collect()
    }
    /// The turn as the screen would run it: every `widgets_every` seconds the next page on.
    fn widget_showing(&self) -> Option<home_companion::ui::widgets::Kind> {
        let on = self.settings.widgets().0;
        let order: Vec<_> = self
            .settings
            .widget_order()
            .into_iter()
            .filter(|k| on.contains(k))
            .collect();
        let every = self.settings.widgets().1.max(5) as u64;
        let n = order.len() as u64;
        (n > 0).then(|| order[((self.born.elapsed().as_secs() / every) % n) as usize])
    }
    fn remote_enabled(&self) -> bool {
        !self.dir.join("screen-off").exists()
    }
    fn remote_hidden(&self) -> Option<&'static str> {
        self.dir.join("screen-private").exists().then_some("camera")
    }
    fn remote_frame(&self) -> Result<Option<Frame>, &'static str> {
        Ok(Some(self.draw()))
    }
    fn remote_touch(&self, t: home_companion::panel::remote::Touch) {
        if t.kind == home_companion::panel::remote::TouchKind::Down {
            eprintln!("screen: touch at {:.3} {:.3}", t.x, t.y);
        }
        self.taps.borrow_mut().push((t.x, t.y, Instant::now()));
    }
    fn remote_home(&self) {
        eprintln!("screen: back to the home");
    }
    fn remote_changed(&self, c: Option<&home_companion::panel::remote::Controller>) {
        match c {
            Some(c) => eprintln!(
                "screen: controlled by {} on {}",
                c.agent.browser, c.agent.os
            ),
            None => eprintln!("screen: nobody in control"),
        }
    }
    fn widget_quotes(&self) -> Vec<QuoteChoice> {
        self.settings.quotes()
    }
    fn widget_cities(&self) -> Vec<CityChoice> {
        self.settings.cities()
    }
    /// Canned hits (the real body asks Yahoo, CoinGecko, Open-Meteo); one name is markup, to
    /// show it stays text.
    fn search_start(&self, id: &str, what: What, q: &str, _lang: &str) -> bool {
        self.started
            .borrow_mut()
            .insert(id.to_string(), Instant::now());
        if matches!(what, What::Sensor | What::Player | What::Person) {
            let f = home_companion::ha::widgets::search(&self.ha_catalog(), what, q);
            self.searches
                .borrow_mut()
                .insert(id.into(), SearchState::Done(f));
            return true;
        }
        let quote = |key: &str, src, id: &str, name: &str, detail: &str| Found {
            hit: Hit {
                key: key.into(),
                name: name.into(),
                detail: detail.into(),
            },
            choice: Choice::Quote(QuoteChoice {
                source: src,
                id: id.into(),
                name: name.into(),
            }),
        };
        let city = |cid: u64, name: &str, region: &str, country: &str, lat, lon| Found {
            hit: Hit {
                key: format!("city:{cid}"),
                name: name.into(),
                detail: format!("{region}, {country}"),
            },
            choice: Choice::City(CityChoice {
                id: cid,
                name: name.into(),
                region: region.into(),
                country: country.into(),
                lat,
                lon,
            }),
        };
        let found = match what {
            What::Quote => vec![
                quote(
                    "yahoo:AAPL",
                    Source::Yahoo,
                    "AAPL",
                    "Apple Inc.",
                    "AAPL · NASDAQ · Equity",
                ),
                quote(
                    "yahoo:APLE",
                    Source::Yahoo,
                    "APLE",
                    "Apple Hospitality REIT, Inc.",
                    "APLE · NYSE · Equity",
                ),
                quote(
                    "yahoo:EVIL",
                    Source::Yahoo,
                    "EVIL",
                    "<img src=x onerror=alert(1)>",
                    "EVIL · Test",
                ),
                quote(
                    "coingecko:bitcoin",
                    Source::Coingecko,
                    "bitcoin",
                    "Bitcoin",
                    "BTC · Crypto",
                ),
            ],
            _ => vec![
                city(4409896, "Springfield", "Missouri", "US", 37.22, -93.3),
                city(4250542, "Springfield", "Illinois", "US", 39.8, -89.64),
                city(4951788, "Springfield", "Massachusetts", "US", 42.1, -72.59),
            ],
        };
        self.searches
            .borrow_mut()
            .insert(id.into(), SearchState::Done(found));
        true
    }
    fn search_state(&self, id: &str) -> SearchState {
        self.searches
            .borrow()
            .get(id)
            .cloned()
            .unwrap_or(SearchState::Unknown)
    }
    fn widget_ha(&self) -> home_companion::ha::widgets::Want {
        home_companion::settings_model::widget_ha(self)
    }
    /// A sample catalogue (invented entities).
    fn ha_catalog(&self) -> Vec<home_companion::ha::widgets::Entity> {
        use home_companion::ha::widgets::Entity;
        let e = |id: &str, name: &str, unit: &str, class: &str| Entity {
            id: id.into(),
            name: name.into(),
            unit: unit.into(),
            device_class: class.into(),
            statistics: true,
        };
        vec![
            e("person.ada", "Ada", "", ""),
            e("person.bob", "Bob", "", ""),
            e("sensor.humidity", "Humidity", "%", "humidity"),
            e("sensor.living_room", "Living room", "°C", "temperature"),
            e(
                "sensor.outdoor_temperature",
                "Outdoor temperature",
                "°C",
                "temperature",
            ),
            e("sensor.power", "Power", "W", "power"),
            e("media_player.speaker", "Speaker", "", ""),
        ]
    }
    fn ha_catalog_refresh(&self) {}
    fn notes(&self) -> Option<Vec<(i64, String)>> {
        Some(self.notes.borrow().clone())
    }
    fn note_add(&self, text: &str) {
        let mut n = self.notes.borrow_mut();
        let id = n.iter().map(|x| x.0).max().unwrap_or(0) + 1;
        n.push((id, text.into()));
    }
    fn note_edit(&self, id: i64, text: &str) {
        if let Some(x) = self.notes.borrow_mut().iter_mut().find(|x| x.0 == id) {
            x.1 = text.into();
        }
    }
    fn note_remove(&self, id: i64) {
        self.notes.borrow_mut().retain(|x| x.0 != id);
    }
    /// The sample pages (the screen's demos), with the choices of this panel: a chosen item that
    /// is not in the samples gets made-up numbers a few seconds after it was added.
    fn widget_views(
        &self,
        lang: &str,
    ) -> Vec<(Kind, Option<home_companion::ui::widgets::WidgetView>, bool)> {
        let l = locale::load_or_english(&locale::repo_locales(), lang).expect("locales");
        let s = samples::samples(&l);
        let w = samples::words(&l);
        let quotes = self.settings.quotes();
        let cities = self.settings.cities();
        let ha = self.widget_ha();
        let catalog = self.ha_catalog();
        let name_of = |id: &str| {
            catalog
                .iter()
                .find(|e| e.id == id)
                .map_or_else(|| id.to_string(), |e| e.name.clone())
        };
        let ready = |key: &str| {
            let mut seen = self.seen.borrow_mut();
            let at = *seen.entry(key.to_string()).or_insert_with(Instant::now);
            at.elapsed() >= Duration::from_secs(3)
        };
        let q_trends: Vec<Trend> = quotes
            .iter()
            .enumerate()
            .filter_map(|(i, q)| {
                let t = s.quotes.iter().find(|t| t.name == q.name).cloned();
                match t {
                    Some(t) => Some(t),
                    None if ready(&q.key()) => {
                        let mut t = s.quotes[i % s.quotes.len()].clone();
                        t.name = q.name.clone();
                        Some(t)
                    }
                    None => None,
                }
            })
            .collect();
        let sensor_names: Vec<String> = ha.sensors.iter().map(|id| name_of(id)).collect();
        let s_trends: Vec<Trend> = sensor_names
            .iter()
            .enumerate()
            .filter(|(_, n)| ready(&format!("sensor:{n}")))
            .map(|(i, n)| {
                let mut t = s.sensors[i % s.sensors.len()].clone();
                t.name = n.clone();
                t
            })
            .collect();
        let c_rows: Vec<CityRow> = cities
            .iter()
            .enumerate()
            .filter(|(_, c)| ready(&c.key()))
            .map(|(i, c)| {
                let mut r = s.cities[i % s.cities.len()].clone();
                r.name = c.name.clone();
                r
            })
            .collect();
        let notes: Vec<String> = self.notes.borrow().iter().map(|n| n.1.clone()).collect();
        let lists = self.lists.borrow().clone();
        let people: Vec<Person> = ha
            .people
            .clone()
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map(|(i, id)| Person {
                name: name_of(id),
                place: if i == 0 { Place::Home } else { Place::Away },
            })
            .collect();
        let real = Sources {
            next: &s.next,
            weather: s.weather.clone(),
            weather_hero: Some(s.weather_hero.clone()),
            shopping: Some(&lists),
            quotes: &q_trends,
            cities: &c_rows,
            notes: &notes,
            sensors: &s_trends,
            check: &[],
            playing: None,
            people: &people,
        };
        let qn: Vec<String> = quotes.iter().map(|q| q.name.clone()).collect();
        let cn: Vec<String> = cities.iter().map(|c| c.name.clone()).collect();
        let player = if ha.player.is_empty() {
            String::new()
        } else {
            name_of(&ha.player)
        };
        let chosen = Chosen {
            quotes: &qn,
            cities: &cn,
            sensors: &sensor_names,
            check: ha.openings || ha.batteries,
            player: &player,
        };
        widgets::panel_pages(
            &real,
            &s.sources(),
            &chosen,
            &w,
            samples::NOW,
            samples::NOW,
            &samples::local,
        )
    }
    fn widget_samples(&self, lang: &str) -> Vec<home_companion::ui::widgets::WidgetView> {
        let l = locale::load_or_english(&locale::repo_locales(), lang).expect("locales");
        let s = samples::samples(&l);
        let w = samples::words(&l);
        Kind::ALL
            .into_iter()
            .filter_map(|k| widgets::page(&s.sources(), k, &w, samples::NOW, &samples::local))
            .collect()
    }
    /// Made-up prices and weather for the canned hits, a second after the names.
    fn search_rows(&self, id: &str, lang: &str) -> Option<Vec<(String, WidgetRow)>> {
        let started = *self.started.borrow().get(id)?;
        if started.elapsed() < Duration::from_secs(1) {
            return None;
        }
        let l = locale::load_or_english(&locale::repo_locales(), lang).expect("locales");
        let s = samples::samples(&l);
        let w = samples::words(&l);
        let SearchState::Done(found) = self.search_state(id) else {
            return None;
        };
        Some(
            found
                .iter()
                .enumerate()
                .filter_map(|(i, f)| {
                    let row = match &f.choice {
                        Choice::Quote(_) => {
                            let mut r = widgets::trend_row(&s.quotes[i % s.quotes.len()], &w);
                            r.text.clear();
                            r
                        }
                        Choice::City(_) => {
                            let c = &s.cities[i % s.cities.len()];
                            WidgetRow {
                                left: c.time.clone(),
                                right: c.temp.clone(),
                                note: c.cond.clone(),
                                ..Default::default()
                            }
                        }
                        Choice::Entity(_) => return None,
                    };
                    Some((f.hit.key.clone(), row))
                })
                .collect(),
        )
    }
    fn search_choice(&self, id: &str, key: &str) -> Option<Choice> {
        match self.searches.borrow().get(id) {
            Some(SearchState::Done(f)) => f
                .iter()
                .find(|x| x.hit.key == key)
                .map(|x| x.choice.clone()),
            _ => None,
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir: PathBuf = args
        .next()
        .unwrap_or_else(|| "/tmp/panel-demo".into())
        .into();
    let refuse = args.next().as_deref() == Some("--refuse");
    std::fs::create_dir_all(&dir).expect("dir");
    let item = |u: &str, t: &str| (u.to_string(), t.to_string());
    let demo = Demo {
        seen: RefCell::new(Default::default()),
        started: RefCell::new(Default::default()),
        notes: RefCell::new(vec![
            (1, "The plumber comes on Tuesday morning".into()),
            (2, "Water the basil".into()),
        ]),
        searches: RefCell::new(Default::default()),
        settings: Settings::in_dir(&dir),
        lists: RefCell::new(vec![
            ShopList {
                entity: "todo.groceries".into(),
                name: "Groceries".into(),
                items: vec![
                    item("u1", "Bread"),
                    item("u2", "Milk"),
                    item("u3", "Coffee"),
                ],
            },
            ShopList {
                entity: "todo.hardware".into(),
                name: "Hardware".into(),
                items: vec![item("u4", "Screws")],
            },
        ]),
        asked: RefCell::new(None),
        volume: RefCell::new(40),
        dir: dir.clone(),
        born: Instant::now(),
        picture: picture(),
        taps: RefCell::new(Vec::new()),
        frame: RefCell::new(None),
    };
    // A first start: some markets, cities and sensors chosen (public names), as after a
    // few minutes with the panel.
    if demo.settings.quotes().is_empty() {
        let q = |source, id: &str, name: &str| QuoteChoice {
            source,
            id: id.into(),
            name: name.into(),
        };
        model::commit(
            &demo,
            Change::Quotes(vec![
                q(Source::Yahoo, "AAPL", "Apple Inc."),
                q(Source::Yahoo, "^GSPC", "S&P 500"),
                q(Source::Coingecko, "bitcoin", "Bitcoin"),
                q(Source::Yahoo, "EURUSD=X", "EUR/USD"),
                q(Source::Yahoo, "GC=F", "Gold"),
            ]),
        );
        let c = |id, name: &str, region: &str, country: &str, lat, lon| CityChoice {
            id,
            name: name.into(),
            region: region.into(),
            country: country.into(),
            lat,
            lon,
        };
        model::commit(
            &demo,
            Change::Cities(vec![
                c(2950159, "Berlin", "Berlin", "DE", 52.52, 13.41),
                c(1850147, "Tokyo", "Tokyo", "JP", 35.69, 139.69),
                c(4250542, "Springfield", "Illinois", "US", 39.8, -89.64),
            ]),
        );
        let mut ha = demo.widget_ha();
        ha.sensors = vec![
            "sensor.outdoor_temperature".into(),
            "sensor.living_room".into(),
            "sensor.humidity".into(),
            "sensor.power".into(),
        ];
        ha.player = "media_player.speaker".into();
        ha.people = Some(vec!["person.ada".into(), "person.bob".into()]);
        model::commit(&demo, Change::WidgetHa(ha));
    }
    // Everything there at once (the delays are for what is added later).
    let long_ago = Instant::now() - Duration::from_secs(10);
    for k in demo
        .settings
        .quotes()
        .iter()
        .map(|q| q.key())
        .chain(demo.settings.cities().iter().map(|c| c.key()))
    {
        demo.seen.borrow_mut().insert(k, long_ago);
    }
    for n in ["Outdoor temperature", "Living room", "Humidity", "Power"] {
        demo.seen
            .borrow_mut()
            .insert(format!("sensor:{n}"), long_ago);
    }
    let mut sessions = Sessions::load(&dir);
    let sock = home_companion::panel::socket::start(&dir.join("panel.sock")).expect("socket");
    eprintln!("panel_body: {}", dir.join("panel.sock").display());
    loop {
        if let Ok(c) = sock.calls.recv_timeout(Duration::from_millis(200)) {
            let now = home_companion::panel::sessions::Now::system();
            let a = protocol::handle(
                &demo,
                &mut sessions,
                &c.request,
                now,
                &mut sessions::system_random,
            );
            let _ = c.reply.send(a);
        }
        let due = demo
            .asked
            .borrow()
            .as_ref()
            .is_some_and(|(_, at)| at.elapsed() >= ALLOW_AFTER);
        if due && let Some((id, _)) = demo.asked.borrow_mut().take() {
            let now = home_companion::panel::sessions::Now::system();
            sessions.answer(&id, !refuse, now, &mut sessions::system_random);
            eprintln!("screen: {}", if refuse { "Refuse" } else { "Allow" });
        }
    }
}
