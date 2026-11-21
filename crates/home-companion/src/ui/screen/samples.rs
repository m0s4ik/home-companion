// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The widget's sample data, the same the demos draw (`--state widget-*`): the browser panel's
//! gallery shows every kind of page with them, and a page that shows only at times (what to
//! check, now playing) is previewed with them. Invented names and numbers, in the locale's words.

use locale::Locale;

use super::{
    DEMO_DATE, DEMO_NOW, demo_cities, demo_events, demo_ha, demo_quotes, local_hm, widget_words,
};
use crate::ha::weather::Weather;
use crate::ui::lists::ShopList;
use crate::ui::next::{self, NextItem};
use crate::ui::widgets::{self, CheckRow, CityRow, Person, Playing, Sources, Trend, WidgetRow};

/// When the samples are (the demos' afternoon).
pub const NOW: i64 = DEMO_NOW as i64;

/// The data of every page, owned.
pub struct Samples {
    pub next: Vec<NextItem>,
    pub weather: Vec<WidgetRow>,
    pub weather_hero: crate::ui::weather::WeatherHero,
    pub shopping: Vec<ShopList>,
    pub quotes: Vec<Trend>,
    pub cities: Vec<CityRow>,
    pub notes: Vec<String>,
    pub sensors: Vec<Trend>,
    pub check: Vec<CheckRow>,
    pub playing: Playing,
    pub people: Vec<Person>,
}

impl Samples {
    pub fn sources(&self) -> Sources<'_> {
        Sources {
            next: &self.next,
            weather: self.weather.clone(),
            weather_hero: Some(self.weather_hero.clone()),
            shopping: Some(&self.shopping),
            quotes: &self.quotes,
            cities: &self.cities,
            notes: &self.notes,
            sensors: &self.sensors,
            check: &self.check,
            playing: Some(&self.playing),
            people: &self.people,
            picks: &[],
        }
    }
}

/// "14:02" and the local day of a time (the screen's clock), for [`widgets::pages`].
pub fn local(t: i64) -> (String, i64) {
    local_hm(t)
}

/// The screen's words of the widget in `l`.
pub fn words(l: &Locale) -> widgets::Words<'_> {
    widget_words(l)
}

/// The other cities as the widget's rows: the time there now, the name, the sky, the
/// temperature.
pub(super) fn city_rows_of(
    choices: &[crate::feeds::CityChoice],
    weather: &[crate::feeds::openmeteo::CityWeather],
    now: i64,
) -> Vec<CityRow> {
    weather
        .iter()
        .filter_map(|w| {
            let c = choices.iter().find(|c| c.id == w.id)?;
            let there = (now + w.utc_offset as i64).rem_euclid(86400);
            Some(CityRow {
                key: c.key(),
                name: c.name.clone(),
                time: format!("{:02}:{:02}", there / 3600, there % 3600 / 60),
                temp: format!("{}°", w.temp.round() as i64),
                cond: w.cond.clone(),
                at: w.at,
                fresh: 2 * crate::feeds::plan::OPENMETEO as i64,
                max_age: 24 * 3600,
            })
        })
        .collect()
}

/// The rows a search's hits will become (hit key → row; the name left to the hit), as the
/// widget writes them.
pub fn hit_rows(
    rows: &crate::feeds::SearchRows,
    w: &widgets::Words,
    now: i64,
) -> Vec<(String, WidgetRow)> {
    let mut out: Vec<(String, WidgetRow)> = rows
        .quotes
        .iter()
        .map(|q| {
            let t = Trend {
                value: q.price,
                decimals: q.decimals,
                unit: q.unit.clone(),
                prev: q.prev,
                pct: true,
                points: q.points.clone(),
                low: q.low,
                high: q.high,
                ..Default::default()
            };
            (q.key.clone(), widgets::trend_row(&t, w))
        })
        .collect();
    for (key, c) in &rows.cities {
        let there = (now + c.utc_offset as i64).rem_euclid(86400);
        out.push((
            key.clone(),
            WidgetRow {
                left: format!("{:02}:{:02}", there / 3600, there % 3600 / 60),
                right: format!("{}°", c.temp.round() as i64),
                note: c.cond.clone(),
                ..Default::default()
            },
        ));
    }
    out
}

/// The samples in the words of `l`.
pub fn samples(l: &Locale) -> Samples {
    let midnight = NOW - NOW % 86400;
    let mut events = demo_events(l);
    let ev = |start: i64, title: &str| crate::brain_link::Upcoming {
        start,
        end: start + 3600,
        all_day: false,
        title: title.into(),
        calendar: "calendar.demo0".into(),
    };
    events.push(ev(midnight + 19 * 3600 + 1800, l.sample("event_dinner")));
    events.push(ev(midnight + 86400 + 9 * 3600, l.sample("event_car")));
    let reminders = vec![(midnight + 18 * 3600, l.sample("reminder_text").to_string())];
    let hour = NOW - NOW % 3600;
    let weather = Weather::sample_week(DEMO_DATE)
        .with_sample_hours(hour)
        .with_sample_sun(NOW / 86400, NOW);
    let columns =
        crate::ui::weather::widget_columns(&weather, l, DEMO_DATE, NOW, &|t| local_hm(t).0);
    let weather_hero = crate::ui::weather::hero(&weather, l, DEMO_DATE, NOW, &local_hm);
    let items: Vec<(String, String)> = l
        .sample("shop_items")
        .split('|')
        .enumerate()
        .map(|(i, t)| (i.to_string(), t.to_string()))
        .collect();
    let shopping = vec![ShopList {
        entity: "todo.groceries".into(),
        name: l
            .sample("list_names")
            .split('|')
            .next()
            .unwrap_or("")
            .into(),
        items,
    }];
    let (ha, curves) = demo_ha(l);
    let sensors = ha
        .sensors
        .iter()
        .map(|s| {
            let c = curves.get(&s.id);
            Trend {
                key: s.id.clone(),
                name: s.name.clone(),
                value: s.value,
                decimals: s.decimals,
                unit: s.unit.clone(),
                prev: c.map(|c| c.first),
                pct: false,
                points: c.map(|c| c.points.clone()).unwrap_or_default(),
                low: c.map_or(s.value, |c| c.low.min(s.value)),
                high: c.map_or(s.value, |c| c.high.max(s.value)),
                at: NOW,
                fresh: i64::MAX / 4,
                max_age: i64::MAX / 4,
            }
        })
        .collect();
    let (choices, cities) = demo_cities();
    Samples {
        next: next::merge(&reminders, &events, NOW),
        weather: columns,
        weather_hero,
        shopping,
        quotes: demo_quotes(),
        cities: city_rows_of(&choices, &cities, NOW),
        notes: l
            .sample("notes_board")
            .split('|')
            .map(String::from)
            .collect(),
        sensors,
        check: ha.check,
        playing: ha.playing.unwrap_or_default(),
        people: ha.people,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every kind has a sample page, in the locale's words.
    #[test]
    fn every_kind_has_a_sample() {
        for lang in ["en", "it"] {
            let l = locale::load_or_english(&locale::repo_locales(), lang).unwrap();
            let s = samples(&l);
            let w = words(&l);
            for k in widgets::Kind::ALL {
                let p = widgets::page(&s.sources(), k, &w, NOW, &local);
                assert!(p.is_some_and(|p| !p.rows.is_empty()), "{lang} {k:?}");
            }
        }
    }
}
