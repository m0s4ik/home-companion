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

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use home_companion::feeds::{
    Choice, CityChoice, Found, Hit, QuoteChoice, SearchState, Source, What,
};
use home_companion::panel::protocol::{self, PanelHost};
use home_companion::panel::sessions::{self, Request, Sessions};
use home_companion::settings::Settings;
use home_companion::settings_model::{Change, Host};
use home_companion::ui::lists::{self, ShopList};
use serde_json::{Value, json};

const ALLOW_AFTER: Duration = Duration::from_secs(4);

struct Demo {
    notes: RefCell<Vec<(i64, String)>>,
    searches: RefCell<std::collections::HashMap<String, SearchState>>,
    settings: Settings,
    lists: RefCell<Vec<ShopList>>,
    asked: RefCell<Option<(String, Instant)>>,
    volume: RefCell<u8>,
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
    fn widget_quotes(&self) -> Vec<QuoteChoice> {
        self.settings.quotes()
    }
    fn widget_cities(&self) -> Vec<CityChoice> {
        self.settings.cities()
    }
    /// Canned hits (the real body asks Yahoo, CoinGecko, Open-Meteo); one name is markup, to
    /// show it stays text.
    fn search_start(&self, id: &str, what: What, _q: &str, _lang: &str) -> bool {
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
            What::City => vec![
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
    };
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
