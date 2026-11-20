// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The widget's data from the internet: the markets chosen from the browser (Yahoo Finance)
//! and the coins (CoinGecko), the other cities' weather (Open-Meteo). One thread, started the
//! first time something is chosen or searched, asks one thing at a time through `curl`
//! ([`http`]) when [`plan`] says it is due, and only while the page is shown (not at night,
//! not with the screen off). What it got is an [`Snapshot`] the screen reads when [`Feeds::rev`]
//! changes, and `feeds.json` next to settings.toml, so the data is on screen at once after a
//! restart. The screen's thread never waits for the network: searches are asked here and their
//! answers picked up later ([`Feeds::search_status`]).

pub mod cache;
pub mod coingecko;
pub mod http;
pub mod openmeteo;
pub mod plan;
pub mod yahoo;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use http::{Fetch, Host, Outcome};
use plan::{Job, YahooHost};

/// Markets at most.
pub const MAX_QUOTES: usize = 8;
/// Other cities at most.
pub const MAX_CITIES: usize = 4;
/// A name from a source, as the screen and the panel show it: at most this many characters.
pub const MAX_NAME: usize = 40;

/// A name from a source or Home Assistant, made safe to show: no control characters, spaces
/// squeezed, at most [`MAX_NAME`] characters.
pub fn clean_name(s: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in s.chars() {
        if c.is_control() || c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        // Bidirectional overrides would turn the rest of a line around.
        if matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}')
        {
            continue;
        }
        if space {
            out.push(' ');
            space = false;
        }
        out.push(c);
    }
    out.chars()
        .take(MAX_NAME)
        .collect::<String>()
        .trim_end()
        .to_string()
}

/// Where a quote comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Yahoo,
    Coingecko,
}

impl Source {
    pub fn key(self) -> &'static str {
        match self {
            Source::Yahoo => "yahoo",
            Source::Coingecko => "coingecko",
        }
    }
}

/// A market chosen from the browser (settings.toml `[[widgets.quotes]]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuoteChoice {
    pub source: Source,
    /// The Yahoo symbol or the CoinGecko id.
    pub id: String,
    /// As the source named it, cleaned.
    pub name: String,
}

impl QuoteChoice {
    /// "yahoo:AAPL", "coingecko:bitcoin": the key of its data and of the panel's `ref`.
    pub fn key(&self) -> String {
        format!("{}:{}", self.source.key(), self.id)
    }

    /// The id is one the source could have given (checked again on every read of the file).
    pub fn valid(&self) -> bool {
        match self.source {
            Source::Yahoo => yahoo::valid_symbol(&self.id),
            Source::Coingecko => valid_coin(&self.id),
        }
    }
}

/// A CoinGecko id ("bitcoin", "usd-coin").
pub fn valid_coin(s: &str) -> bool {
    (1..=64).contains(&s.len())
        && s.as_bytes()[0] != b'-'
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// A city chosen from the browser (settings.toml `[[widgets.cities]]`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CityChoice {
    /// The geocoding's id.
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub country: String,
    /// Rounded to two decimals (about a kilometre: enough, and it says less).
    pub lat: f64,
    pub lon: f64,
}

impl CityChoice {
    pub fn key(&self) -> String {
        format!("city:{}", self.id)
    }

    pub fn valid(&self) -> bool {
        self.lat.is_finite()
            && self.lon.is_finite()
            && (-90.0..=90.0).contains(&self.lat)
            && (-180.0..=180.0).contains(&self.lon)
    }
}

/// The last price of a market or a coin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Quote {
    /// [`QuoteChoice::key`].
    pub key: String,
    pub price: f64,
    /// Yesterday's close (a coin: 24 hours ago); None: no change.
    pub prev: Option<f64>,
    /// The currency's code ("USD"); "" for an index or a currency pair.
    pub unit: String,
    pub decimals: u8,
    /// The day, at most 32 points (empty: no curve), and its low and high.
    pub points: Vec<f32>,
    pub low: f64,
    pub high: f64,
    /// Its market is open now (Yahoo; coins always).
    pub market_open: bool,
    /// When it was fetched (unix seconds).
    pub at: i64,
}

/// What the feeds have now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(default)]
    pub quotes: Vec<Quote>,
    #[serde(default)]
    pub cities: Vec<openmeteo::CityWeather>,
}

impl Snapshot {
    pub fn city(&self, id: u64) -> Option<&openmeteo::CityWeather> {
        self.cities.iter().find(|c| c.id == id)
    }

    pub fn quote(&self, key: &str) -> Option<&Quote> {
        self.quotes.iter().find(|q| q.key == key)
    }

    fn put_quote(&mut self, q: Quote) {
        match self.quotes.iter_mut().find(|x| x.key == q.key) {
            Some(x) => *x = q,
            None => self.quotes.push(q),
        }
    }
}

/// What the feeds fetch: the choices and the currency of the coins.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FeedsConfig {
    pub quotes: Vec<QuoteChoice>,
    /// The coins' currency ("usd"; [`coingecko::CURRENCIES`]).
    pub currency: String,
    pub cities: Vec<CityChoice>,
    /// The cities' temperatures in °F (as the home's weather).
    pub fahrenheit: bool,
}

impl FeedsConfig {
    pub fn is_empty(&self) -> bool {
        self.quotes.is_empty() && self.cities.is_empty()
    }
}

/// A search's result: what the panel shows, and the choice it becomes when added.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// The `ref` the panel sends back ("yahoo:AAPL").
    pub key: String,
    pub name: String,
    /// "AAPL · NASDAQ · Equity".
    pub detail: String,
}

enum Cmd {
    Config(FeedsConfig),
    /// Whether the markets' page and the cities' may fetch now.
    Active(Active),
    Shutdown,
}

/// Which pages may fetch now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Active {
    pub quotes: bool,
    pub cities: bool,
}

/// The feeds' thread seen from the screen.
pub struct Feeds {
    tx: Option<Sender<Cmd>>,
    shared: Arc<Shared>,
    cache: Option<PathBuf>,
    /// What the thread was last told (it is told only changes).
    config: FeedsConfig,
    active: Active,
    fetch: Option<Box<dyn Fetch>>,
    /// The longest spread of the requests when the feeds are needed again (20 s; 0 in tests).
    spread: f64,
}

struct Shared {
    snapshot: Mutex<Snapshot>,
    rev: AtomicU64,
}

impl Feeds {
    /// The feeds with their cache in `dir` (read now: the data shows at once); no thread yet.
    pub fn new(dir: Option<PathBuf>) -> Feeds {
        Feeds::with_fetch(dir, Box::new(http::Curl::default()))
    }

    pub fn with_fetch(dir: Option<PathBuf>, fetch: Box<dyn Fetch>) -> Feeds {
        let cache = dir.map(|d| d.join("feeds.json"));
        let snapshot = cache.as_deref().map(cache::read).unwrap_or_default();
        Feeds {
            tx: None,
            shared: Arc::new(Shared {
                snapshot: Mutex::new(snapshot),
                rev: AtomicU64::new(1),
            }),
            cache,
            config: FeedsConfig::default(),
            active: Active::default(),
            fetch: Some(fetch),
            spread: 20.0,
        }
    }

    /// Grows each time the data changes.
    pub fn rev(&self) -> u64 {
        self.shared.rev.load(Ordering::Acquire)
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared
            .snapshot
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default()
    }

    /// The thread, started the first time it is needed.
    fn sender(&mut self) -> Option<&Sender<Cmd>> {
        if self.tx.is_none() {
            let fetch = self.fetch.take()?;
            let (tx, rx) = channel();
            let shared = self.shared.clone();
            let cache = self.cache.clone();
            let spread = self.spread;
            let spawned = std::thread::Builder::new()
                .name("feeds".into())
                .spawn(move || {
                    let mut w = Worker::new(fetch, shared, cache);
                    w.spread = spread;
                    w.run(rx)
                });
            match spawned {
                Ok(_) => {
                    log::info!(event = "feeds"; "feeds: started");
                    self.tx = Some(tx);
                }
                Err(e) => log::warn!(event = "feeds"; "feeds: no thread: {e}"),
            }
        }
        self.tx.as_ref()
    }

    /// The choices changed (or are read at start): the thread starts if there is something to
    /// fetch.
    pub fn configure(&mut self, config: FeedsConfig) {
        if config == self.config {
            return;
        }
        self.config = config.clone();
        if self.tx.is_none() && config.is_empty() {
            return;
        }
        if let Some(tx) = self.sender() {
            let _ = tx.send(Cmd::Config(config));
        }
    }

    /// Whether the markets and the cities may be fetched now (their page on, not the night, the
    /// screen on).
    pub fn set_active(&mut self, on: Active) {
        if on == self.active {
            return;
        }
        self.active = on;
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Active(on));
        }
    }
}

impl Drop for Feeds {
    fn drop(&mut self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Cmd::Shutdown);
        }
    }
}

/// The thread's state: the jobs and what it fetched.
struct Worker {
    fetch: Box<dyn Fetch>,
    shared: Arc<Shared>,
    cache: Option<PathBuf>,
    config: FeedsConfig,
    active: Active,
    /// One job per Yahoo symbol, by the choice's key.
    yahoo: HashMap<String, Job>,
    /// All the coins in one request.
    coins: Job,
    /// All the cities in one request.
    weather: Job,
    host: YahooHost,
    /// The last Yahoo request (requests are [`plan::YAHOO_GAP`] apart).
    last_yahoo: f64,
    start: Instant,
    spread: f64,
}

impl Worker {
    fn new(fetch: Box<dyn Fetch>, shared: Arc<Shared>, cache: Option<PathBuf>) -> Worker {
        Worker {
            fetch,
            shared,
            cache,
            config: FeedsConfig::default(),
            active: Active::default(),
            yahoo: HashMap::new(),
            coins: Job::new(),
            weather: Job::new(),
            host: YahooHost::default(),
            last_yahoo: f64::NEG_INFINITY,
            start: Instant::now(),
            spread: 20.0,
        }
    }

    fn clock(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        loop {
            let now = self.clock();
            let wait = self
                .next(now)
                .map_or(3600.0, |t| (t - now).clamp(0.05, 3600.0));
            match rx.recv_timeout(Duration::from_secs_f64(wait)) {
                Ok(Cmd::Config(c)) => self.configure(c, self.clock()),
                Ok(Cmd::Active(on)) => self.set_active(on, self.clock()),
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            let now = self.clock();
            self.step(now, now_unix());
        }
    }

    fn configure(&mut self, c: FeedsConfig, now: f64) {
        let keys: Vec<String> = c
            .quotes
            .iter()
            .filter(|q| q.source == Source::Yahoo)
            .map(|q| q.key())
            .collect();
        self.yahoo.retain(|k, _| keys.contains(k));
        for k in &keys {
            self.yahoo.entry(k.clone()).or_default();
        }
        // A new coin or another currency: the coins at once.
        let coins = |c: &FeedsConfig| -> Vec<String> {
            c.quotes
                .iter()
                .filter(|q| q.source == Source::Coingecko)
                .map(|q| q.key())
                .collect()
        };
        let new_currency = c.currency != self.config.currency;
        if new_currency || coins(&c).iter().any(|k| !coins(&self.config).contains(k)) {
            self.coins = Job::new();
        }
        // What is no longer chosen (and the coins in another currency) leaves the data and the
        // cache.
        let all: Vec<String> = c
            .quotes
            .iter()
            .filter(|q| !(new_currency && q.source == Source::Coingecko))
            .map(|q| q.key())
            .collect();
        // Another city or another unit: the cities at once.
        if c.cities != self.config.cities || c.fahrenheit != self.config.fahrenheit {
            self.weather = Job::new();
        }
        let ids: Vec<u64> = c.cities.iter().map(|x| x.id).collect();
        let unit = if c.fahrenheit { "°F" } else { "°C" };
        let changed = {
            let mut s = self
                .shared
                .snapshot
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let before = (s.quotes.len(), s.cities.len());
            s.quotes.retain(|q| all.contains(&q.key));
            s.cities.retain(|w| ids.contains(&w.id) && w.unit == unit);
            (s.quotes.len(), s.cities.len()) != before
        };
        self.config = c;
        if changed {
            self.publish();
        }
        self.wake(now);
    }

    fn set_active(&mut self, on: Active, now: f64) {
        self.active = on;
        self.wake(now);
    }

    /// Back to work: what is overdue goes within 20 s, spread.
    fn wake(&mut self, now: f64) {
        let spread = self.spread;
        for j in self
            .yahoo
            .values_mut()
            .chain(std::iter::once(&mut self.coins))
        {
            j.wake(now, (plan::jitter() + 1.0) / 2.0 * spread);
        }
    }

    /// The chosen coins' ids.
    fn coin_ids(&self) -> Vec<String> {
        self.config
            .quotes
            .iter()
            .filter(|q| q.source == Source::Coingecko)
            .map(|q| q.id.clone())
            .collect()
    }

    /// The next time something is due (None: nothing to do).
    fn next(&self, now: f64) -> Option<f64> {
        let q = self.active.quotes;
        let coins = (q && !self.coin_ids().is_empty()).then_some(self.coins.due);
        let weather =
            (self.active.cities && !self.config.cities.is_empty()).then_some(self.weather.due);
        self.yahoo
            .values()
            .filter(|_| q)
            .map(|j| j.due.max(self.last_yahoo + plan::YAHOO_GAP))
            .chain(coins)
            .chain(weather)
            .fold(None, |a: Option<f64>, t| Some(a.map_or(t, |a| a.min(t))))
            .map(|t| t.max(now))
    }

    /// Does at most one request that is due. `unix` dates the data.
    fn step(&mut self, now: f64, unix: i64) -> bool {
        if self.active.cities && self.weather.is_due(now) && !self.config.cities.is_empty() {
            self.fetch_weather(now, unix);
            return true;
        }
        if !self.active.quotes {
            return false;
        }
        if self.coins.is_due(now) && !self.coin_ids().is_empty() {
            self.fetch_coins(now, unix);
            return true;
        }
        if now < self.last_yahoo + plan::YAHOO_GAP {
            return false;
        }
        let due = self
            .yahoo
            .iter()
            .filter(|(_, j)| j.is_due(now))
            .min_by(|a, b| a.1.due.total_cmp(&b.1.due))
            .map(|(k, _)| k.clone());
        let Some(key) = due else {
            return false;
        };
        self.fetch_yahoo(&key, now, unix);
        true
    }

    fn fetch_yahoo(&mut self, key: &str, now: f64, unix: i64) {
        let symbol = key.trim_start_matches("yahoo:");
        let host = if self.host.second(now) {
            Host::Yahoo2
        } else {
            Host::Yahoo
        };
        self.last_yahoo = now;
        let outcome = self.fetch.get(&yahoo::chart_url(host, symbol));
        let (result, retry, refused) = match outcome {
            Outcome::Ok(body) => (yahoo::parse_chart(&body, key, unix), None, false),
            Outcome::Http(code, retry) => (
                Err(format!("HTTP {code}")),
                retry.map(|r| r as f64),
                matches!(code, 401 | 403 | 429),
            ),
            Outcome::Timeout => (Err("timeout".into()), None, false),
            Outcome::Failed(e) => (Err(e), None, false),
        };
        let ok = result.is_ok();
        if self.host.answer(now, ok, refused) {
            log::warn!(event = "feeds", source = "yahoo"; "feeds: Yahoo's first host refused us: its second one for six hours");
        }
        let Some(job) = self.yahoo.get_mut(key) else {
            return;
        };
        match result {
            Ok(q) => {
                let interval = if q.market_open {
                    plan::YAHOO_OPEN
                } else {
                    plan::YAHOO_CLOSED
                };
                job.success(now, interval, plan::jitter());
                log::debug!(event = "feeds", source = "yahoo"; "feeds: {key} {}", q.price);
                let mut s = self
                    .shared
                    .snapshot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                s.put_quote(q);
                drop(s);
                self.publish();
            }
            Err(e) => {
                job.failure(now, plan::YAHOO_OPEN, retry, plan::jitter());
                log::info!(event = "feeds", source = "yahoo", errors = job.errors; "feeds: {key}: {e}");
            }
        }
    }

    fn fetch_coins(&mut self, now: f64, unix: i64) {
        let ids = self.coin_ids();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let currency = self.config.currency.clone();
        let (result, retry) = match self.fetch.get(&coingecko::markets_url(&ids, &currency)) {
            Outcome::Ok(body) => (coingecko::parse_markets(&body, &currency, unix), None),
            Outcome::Http(code, retry) => (Err(format!("HTTP {code}")), retry.map(|r| r as f64)),
            Outcome::Timeout => (Err("timeout".into()), None),
            Outcome::Failed(e) => (Err(e), None),
        };
        match result {
            Ok(quotes) => {
                self.coins.success(now, plan::COINGECKO, plan::jitter());
                let mut s = self
                    .shared
                    .snapshot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                for q in quotes {
                    s.put_quote(q);
                }
                drop(s);
                self.publish();
            }
            Err(e) => {
                self.coins
                    .failure(now, plan::COINGECKO, retry, plan::jitter());
                log::info!(event = "feeds", source = "coingecko", errors = self.coins.errors; "feeds: coins: {e}");
            }
        }
    }

    fn fetch_weather(&mut self, now: f64, unix: i64) {
        let cities = self.config.cities.clone();
        let url = openmeteo::forecast_url(&cities, self.config.fahrenheit);
        let (result, retry) = match self.fetch.get(&url) {
            Outcome::Ok(body) => (openmeteo::parse_forecast(&body, &cities, unix), None),
            Outcome::Http(code, retry) => (Err(format!("HTTP {code}")), retry.map(|r| r as f64)),
            Outcome::Timeout => (Err("timeout".into()), None),
            Outcome::Failed(e) => (Err(e), None),
        };
        match result {
            Ok(list) => {
                self.weather.success(now, plan::OPENMETEO, plan::jitter());
                let mut s = self
                    .shared
                    .snapshot
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                for w in list {
                    match s.cities.iter_mut().find(|x| x.id == w.id) {
                        Some(x) => *x = w,
                        None => s.cities.push(w),
                    }
                }
                drop(s);
                self.publish();
            }
            Err(e) => {
                self.weather
                    .failure(now, plan::OPENMETEO, retry, plan::jitter());
                log::info!(event = "feeds", source = "openmeteo", errors = self.weather.errors; "feeds: cities: {e}");
            }
        }
    }

    /// New data: the revision grows and the cache is written.
    fn publish(&self) {
        self.shared.rev.fetch_add(1, Ordering::AcqRel);
        if let Some(path) = &self.cache {
            let s = self
                .shared
                .snapshot
                .lock()
                .map(|s| s.clone())
                .unwrap_or_default();
            cache::write(path, &s);
        }
    }
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    pub fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/feeds")
                .join(name),
        )
        .unwrap()
    }

    /// Answers by URL, and remembers what was asked.
    #[derive(Clone, Default)]
    pub struct FakeFetch {
        pub asked: Arc<StdMutex<Vec<String>>>,
        pub answers: Arc<StdMutex<Vec<(String, Outcome)>>>,
    }

    impl FakeFetch {
        pub fn answer(&self, url_part: &str, o: Outcome) {
            self.answers.lock().unwrap().insert(0, (url_part.into(), o));
        }
        pub fn asked(&self) -> Vec<String> {
            self.asked.lock().unwrap().clone()
        }
    }

    impl Fetch for FakeFetch {
        fn get(&mut self, url: &http::Url) -> Outcome {
            self.asked.lock().unwrap().push(url.as_str().to_string());
            self.answers
                .lock()
                .unwrap()
                .iter()
                .find(|(p, _)| url.as_str().contains(p.as_str()))
                .map(|(_, o)| o.clone())
                .unwrap_or(Outcome::Http(404, None))
        }
    }

    const QUOTES: Active = Active {
        quotes: true,
        cities: false,
    };

    fn worker(f: &FakeFetch) -> Worker {
        Worker::new(
            Box::new(f.clone()),
            Arc::new(Shared {
                snapshot: Mutex::new(Snapshot::default()),
                rev: AtomicU64::new(1),
            }),
            None,
        )
    }

    fn yahoo(id: &str) -> QuoteChoice {
        QuoteChoice {
            source: Source::Yahoo,
            id: id.into(),
            name: id.into(),
        }
    }

    #[test]
    fn names_are_cleaned() {
        assert_eq!(clean_name("  Apple\n\tInc.\u{0}  "), "Apple Inc.");
        assert_eq!(clean_name("a\u{202e}b"), "ab");
        assert_eq!(clean_name(&"x".repeat(41)).len(), 40);
        assert_eq!(clean_name("Zürich 🌧"), "Zürich 🌧");
        assert_eq!(clean_name("\u{7}"), "");
    }

    #[test]
    fn nothing_is_asked_while_not_shown_then_one_at_a_time() {
        let f = FakeFetch::default();
        f.answer("chart/AAPL", Outcome::Ok(fixture("yahoo-chart-aapl.json")));
        f.answer(
            "chart/%5EGSPC",
            Outcome::Ok(fixture("yahoo-chart-gspc.json")),
        );
        let mut w = worker(&f);
        w.configure(
            FeedsConfig {
                quotes: vec![yahoo("AAPL"), yahoo("^GSPC")],
                ..Default::default()
            },
            0.0,
        );
        // Not shown: nothing.
        assert!(!w.step(100.0, 1_795_198_800));
        assert!(w.next(100.0).is_none());
        w.set_active(QUOTES, 100.0);
        // Overdue: within 20 s, never at once for all.
        let first = w.next(100.0).unwrap();
        assert!((100.0..=120.0).contains(&first), "{first}");
        assert!(w.step(130.0, 1_795_198_800));
        // Three seconds between two Yahoo requests.
        assert!(!w.step(131.0, 1_795_198_800));
        assert!(w.step(133.0, 1_795_198_800));
        assert!(!w.step(140.0, 1_795_198_800));
        assert_eq!(f.asked().len(), 2);
        let s = w.shared.snapshot.lock().unwrap().clone();
        assert_eq!(s.quotes.len(), 2);
        assert_eq!(s.quote("yahoo:AAPL").unwrap().price, 345.49);
        assert!(w.shared.rev.load(Ordering::Acquire) >= 3);
        // Open market: the next one in 15 minutes ±10%.
        let due = w.yahoo["yahoo:AAPL"].due;
        assert!((130.0 + 810.0..=130.0 + 990.0).contains(&due), "{due}");
        // A symbol removed leaves the data.
        w.configure(
            FeedsConfig {
                quotes: vec![yahoo("AAPL")],
                ..Default::default()
            },
            200.0,
        );
        assert_eq!(w.shared.snapshot.lock().unwrap().quotes.len(), 1);
    }

    #[test]
    fn errors_back_off_and_yahoo_falls_back_to_its_second_host() {
        let f = FakeFetch::default();
        f.answer("query1.finance.yahoo.com", Outcome::Http(429, Some(120)));
        f.answer(
            "query2.finance.yahoo.com/v8/finance/chart/AAPL",
            Outcome::Ok(fixture("yahoo-chart-aapl.json")),
        );
        let mut w = worker(&f);
        w.configure(
            FeedsConfig {
                quotes: vec![yahoo("AAPL")],
                ..Default::default()
            },
            0.0,
        );
        w.set_active(QUOTES, 0.0);
        assert!(w.step(30.0, 0));
        // Refused: Retry-After (120 s) is shorter than the interval: the interval.
        let due = w.yahoo["yahoo:AAPL"].due;
        assert!((30.0 + 810.0..=30.0 + 990.0).contains(&due), "{due}");
        assert!(w.host.second(31.0));
        assert!(w.step(due, 0));
        let asked = f.asked();
        assert!(asked[0].starts_with("https://query1.finance.yahoo.com/"));
        assert!(asked[1].starts_with("https://query2.finance.yahoo.com/"));
        assert_eq!(w.yahoo["yahoo:AAPL"].errors, 0);
        // Garbage is an error, never half a quote.
        let g = FakeFetch::default();
        g.answer("chart/AAPL", Outcome::Ok(b"<html>".to_vec()));
        let mut w = worker(&g);
        w.configure(
            FeedsConfig {
                quotes: vec![yahoo("AAPL")],
                ..Default::default()
            },
            0.0,
        );
        w.set_active(QUOTES, 0.0);
        assert!(w.step(30.0, 0));
        assert!(w.shared.snapshot.lock().unwrap().quotes.is_empty());
        assert_eq!(w.yahoo["yahoo:AAPL"].errors, 1);
    }

    #[test]
    fn the_coins_in_one_request_and_again_in_another_currency() {
        let f = FakeFetch::default();
        f.answer(
            "coins/markets",
            Outcome::Ok(fixture("coingecko-markets-eur.json")),
        );
        let mut w = worker(&f);
        let coin = |id: &str| QuoteChoice {
            source: Source::Coingecko,
            id: id.into(),
            name: id.into(),
        };
        let mut c = FeedsConfig {
            quotes: vec![coin("bitcoin"), coin("ethereum"), yahoo("AAPL")],
            currency: "eur".into(),
            ..Default::default()
        };
        w.configure(c.clone(), 0.0);
        w.set_active(QUOTES, 0.0);
        assert!(w.step(25.0, 9));
        assert!(f.asked()[0].contains("vs_currency=eur&ids=bitcoin%2Cethereum&sparkline=true"));
        assert_eq!(w.shared.snapshot.lock().unwrap().quotes.len(), 2);
        // Ten minutes, whatever Yahoo does meanwhile.
        assert!((25.0 + 540.0..=25.0 + 660.0).contains(&w.coins.due));
        // Another currency: the coins' data goes and they are due at once.
        c.currency = "usd".into();
        w.configure(c, 30.0);
        assert!(w.shared.snapshot.lock().unwrap().quotes.is_empty());
        assert!(w.coins.due <= 50.0, "within the 20 s of a wake");
    }

    #[test]
    fn the_cities_in_one_request_only_while_their_page_is_shown() {
        let f = FakeFetch::default();
        f.answer(
            "v1/forecast",
            Outcome::Ok(fixture("openmeteo-forecast-two.json")),
        );
        let mut w = worker(&f);
        let city = |id: u64, lat: f64, lon: f64| CityChoice {
            id,
            name: "x".into(),
            region: String::new(),
            country: String::new(),
            lat,
            lon,
        };
        let c = FeedsConfig {
            cities: vec![city(1, 52.52, 13.41), city(2, 39.8, -89.64)],
            ..Default::default()
        };
        w.configure(c.clone(), 0.0);
        w.set_active(QUOTES, 0.0);
        assert!(!w.step(100.0, 0), "the markets' page is not the cities'");
        w.set_active(
            Active {
                quotes: false,
                cities: true,
            },
            100.0,
        );
        assert!(w.step(125.0, 0));
        assert_eq!(w.shared.snapshot.lock().unwrap().cities.len(), 2);
        assert!((125.0 + 1620.0..=125.0 + 1980.0).contains(&w.weather.due));
        // In °F: the °C data goes, asked again.
        w.configure(
            FeedsConfig {
                fahrenheit: true,
                ..c
            },
            130.0,
        );
        assert!(w.shared.snapshot.lock().unwrap().cities.is_empty());
        assert!(w.weather.due <= 150.0);
    }

    #[test]
    fn choices_are_checked() {
        assert!(yahoo("AAPL").valid());
        assert!(!yahoo("../x").valid());
        let c = QuoteChoice {
            source: Source::Coingecko,
            id: "bitcoin".into(),
            name: "Bitcoin".into(),
        };
        assert!(c.valid());
        assert_eq!(c.key(), "coingecko:bitcoin");
        assert!(!valid_coin("Bit coin") && !valid_coin("-x") && !valid_coin(""));
    }

    /// The thread itself: started on the first choice, quiet until shown, data out, stopped
    /// when dropped.
    #[test]
    fn the_thread_end_to_end() {
        let f = FakeFetch::default();
        f.answer("chart/AAPL", Outcome::Ok(fixture("yahoo-chart-aapl.json")));
        let dir = std::env::temp_dir().join(format!("hc-feeds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut feeds = Feeds::with_fetch(Some(dir.clone()), Box::new(f.clone()));
        feeds.spread = 0.0;
        feeds.configure(FeedsConfig::default());
        assert!(feeds.tx.is_none(), "nothing chosen: no thread");
        feeds.configure(FeedsConfig {
            quotes: vec![yahoo("AAPL")],
            ..Default::default()
        });
        assert!(feeds.tx.is_some());
        std::thread::sleep(Duration::from_millis(100));
        assert!(f.asked().is_empty());
        let rev = feeds.rev();
        feeds.set_active(QUOTES);
        let t0 = Instant::now();
        while feeds.rev() == rev && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(feeds.snapshot().quote("yahoo:AAPL").is_some());
        // The cache, read by a new one.
        let again = Feeds::with_fetch(Some(dir.clone()), Box::new(FakeFetch::default()));
        assert!(again.snapshot().quote("yahoo:AAPL").is_some());
        drop(feeds);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
