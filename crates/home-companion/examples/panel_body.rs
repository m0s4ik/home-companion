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

use home_companion::panel::protocol::{self, PanelHost};
use home_companion::panel::sessions::{self, Request, Sessions};
use home_companion::settings::Settings;
use home_companion::settings_model::{Change, Host};
use home_companion::ui::lists::{self, ShopList};
use serde_json::{Value, json};

const ALLOW_AFTER: Duration = Duration::from_secs(4);

struct Demo {
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
