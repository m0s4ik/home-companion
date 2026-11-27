// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The weather as the home screen writes it (the date line of companion.slint):
//! icon from the condition, temperatures with the language's decimal separator, the condition
//! in words.

use locale::Locale;

use crate::ha::weather::{Sun, Weather, condition_icon};

/// What the weather box shows, ready for the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeatherText {
    /// MDI path data.
    pub icon: &'static str,
    /// "Cloudy" / "Nuvoloso".
    pub condition: String,
    /// "13.7°C" / "13,7°C".
    pub temperature: String,
    /// "14°": the home's date line (rounded, no unit).
    pub rounded: String,
    /// "15°" (empty without today's forecast).
    pub high: String,
    pub low: String,
}

/// One decimal when there is one ("13.7", "14"), with `sep` as the decimal separator.
pub fn number(v: f64, sep: &str) -> String {
    let r = (v * 10.0).round() / 10.0;
    // No "-0".
    let r = if r == 0.0 { 0.0 } else { r };
    let s = if r.fract() == 0.0 {
        format!("{r:.0}")
    } else {
        format!("{r:.1}")
    };
    s.replace('.', sep)
}

/// The locale's key for a condition's words.
fn condition_key(condition: &str) -> String {
    format!("weather_{}", condition.replace('-', "_"))
}

pub fn text(w: &Weather, l: &Locale) -> WeatherText {
    let sep = match l.text("decimal_separator") {
        "decimal_separator" => ".",
        s => s,
    };
    let key = condition_key(&w.condition);
    let condition = match l.text(&key) {
        k if k == key => l.text("weather_exceptional").to_string(),
        s => s.to_string(),
    };
    let whole = |v: Option<f64>| v.map_or(String::new(), |v| format!("{}°", v.round() as i64));
    let (high, low) = match &w.today {
        Some(d) => (whole(d.high), whole(d.low)),
        None => (String::new(), String::new()),
    };
    WeatherText {
        icon: condition_icon(&w.condition),
        condition,
        temperature: format!("{}{}", number(w.temperature, sep), w.unit),
        rounded: format!("{}°", w.temperature.round() as i64),
        high,
        low,
    }
}

/// Another city's page (feeds/): the cities' names (its chips), the one shown, its weather as
/// the weather page shows it, the condition's line with the time there, the source's credit.
pub struct CityView<'a> {
    pub title: &'a str,
    pub names: &'a [String],
    pub index: usize,
    pub text: &'a WeatherText,
    pub condition: &'a str,
    pub rows: &'a [ForecastRow],
    pub footer: &'a str,
}

/// A line of the weather page's forecast.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForecastRow {
    /// "Today", "Tomorrow", "Wednesday 11".
    pub day: String,
    /// Home Assistant's condition (the icon is drawn from it).
    pub kind: String,
    pub condition: String,
    pub high: String,
    pub low: String,
    /// "60%" or "2.5 mm"; empty when dry or unknown.
    pub rain: String,
}

/// Days since 1970-01-01 of a civil date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// "2026-11-08" → (days since the epoch, day of month).
fn parse_date(date: &str) -> Option<(i64, i64)> {
    let mut p = date.get(..10)?.split('-');
    let (y, m, d): (i64, i64, i64) = (
        p.next()?.parse().ok()?,
        p.next()?.parse().ok()?,
        p.next()?.parse().ok()?,
    );
    ((1..=12).contains(&m) && (1..=31).contains(&d)).then(|| (days_from_civil(y, m, d), d))
}

/// "LUNEDÌ" → "Lunedì".
fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| {
            f.to_uppercase()
                .chain(c.flat_map(char::to_lowercase))
                .collect()
        })
        .unwrap_or_default()
}

fn day_label(date: &str, today: &str, l: &Locale) -> String {
    let (Some((n, dom)), Some((t, _))) = (parse_date(date), parse_date(today)) else {
        return date.to_string();
    };
    match n - t {
        0 => l.text("weather_today").to_string(),
        1 => l.text("weather_tomorrow").to_string(),
        _ => {
            // 1970-01-01 was a Thursday; `days` starts from Sunday.
            let wd = (n + 4).rem_euclid(7) as usize;
            let name = l.days.get(wd).map(|d| title_case(d)).unwrap_or_default();
            format!("{name} {dom}")
        }
    }
}

/// The daily forecast for the weather page: today (when Home Assistant has it) and the days
/// after, at most `max` rows. `today` is the local date, "2026-11-08".
pub fn forecast_rows(w: &Weather, l: &Locale, today: &str, max: usize) -> Vec<ForecastRow> {
    let sep = match l.text("decimal_separator") {
        "decimal_separator" => ".",
        s => s,
    };
    w.today
        .iter()
        .chain(&w.next_days)
        .take(max)
        .map(|d| {
            let whole =
                |v: Option<f64>| v.map_or(String::new(), |v| format!("{}°", v.round() as i64));
            let rain = match (d.precipitation_probability, d.precipitation) {
                (Some(p), _) if p >= 1.0 => format!("{}%", p.round() as i64),
                (_, Some(mm)) if mm > 0.0 => format!("{} mm", number(mm, sep)),
                _ => String::new(),
            };
            let key = condition_key(&d.condition);
            ForecastRow {
                day: day_label(&d.date, today, l),
                kind: d.condition.clone(),
                condition: match l.text(&key) {
                    k if k == key => l.text("weather_exceptional").to_string(),
                    s => s.to_string(),
                },
                high: whole(d.high),
                low: whole(d.low),
                rain,
            }
        })
        .collect()
}

/// "Mer 11": a day's short name (the locale's first three letters) and its day of the month.
fn short_day(date: &str, l: &Locale) -> String {
    let Some((n, dom)) = parse_date(date) else {
        return date.to_string();
    };
    let wd = (n + 4).rem_euclid(7) as usize;
    let name: String = l
        .days
        .get(wd)
        .map(|d| title_case(d).chars().take(3).collect())
        .unwrap_or_default();
    format!("{name} {dom}")
}

/// The home widget's weather columns (widgets.rs: `left` the hour or the day, `note` the
/// condition, `text` the temperature): the next four hours from the one going on (`now`,
/// seconds since the epoch; `hm` writes a time "15:00"); without an hourly forecast the days
/// after `today` ("2026-11-08"), at most four, with their high.
pub fn widget_columns(
    w: &Weather,
    l: &Locale,
    today: &str,
    now: i64,
    hm: &dyn Fn(i64) -> String,
) -> Vec<super::widgets::WidgetRow> {
    use super::widgets::WidgetRow;
    let hours: Vec<WidgetRow> = w
        .hours
        .iter()
        .filter(|h| h.at + 3600 > now)
        .filter_map(|h| {
            Some(WidgetRow {
                left: hm(h.at),
                text: format!("{}°", h.temperature?.round() as i64),
                note: h.condition.clone(),
                ..Default::default()
            })
        })
        .take(4)
        .collect();
    if hours.len() >= 2 {
        return hours;
    }
    w.today
        .iter()
        .chain(&w.next_days)
        .filter(|d| d.date.as_str() > today)
        .filter_map(|d| {
            Some(WidgetRow {
                left: short_day(&d.date, l),
                text: format!("{}°", d.high?.round() as i64),
                note: d.condition.clone(),
                ..Default::default()
            })
        })
        .take(4)
        .collect()
}

/// The widget's big block: what the weather does next, in words, now or tomorrow's once the
/// evening has come. The temperature now is already on the date's line above the widget, so
/// the block leads with a sentence and keeps the numbers small.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WeatherHero {
    /// Tomorrow's forecast (else now).
    pub tomorrow: bool,
    /// "Nuvoloso, pioggia dalle 17:00", "Domani pioggia".
    pub label: String,
    /// Home Assistant's condition (the big icon).
    pub kind: String,
    /// The temperature now or tomorrow's high ("14°"; the panel's data, the screen does not draw it).
    pub temp: String,
    /// Under the sentence, small: "Oggi tra 8° e 15°", "Tra 9° e 12°"; empty when unknown.
    pub range: String,
}

/// The sun's times are trusted for the day and the evening only when both are in the next day
/// and a half (else: polar day or night, or old data).
const SUN_HORIZON: i64 = 36 * 3600;

/// Minutes of "15:30".
fn minutes(hm: &str) -> Option<u32> {
    let (h, m) = hm.split_once(':')?;
    Some(h.parse::<u32>().ok()? * 60 + m.parse::<u32>().ok()?)
}

/// It is evening: the sun has set today (the next sunrise is on a later local day). Without
/// usable sun data (no `sun.sun`, polar day or night, old times) evening is from 18:00. After
/// midnight it is a new day: "now" again. `local` writes a time as ("15:30", local day number).
pub fn evening(sun: Option<&Sun>, now: i64, local: &dyn Fn(i64) -> (String, i64)) -> bool {
    let (hm, today) = local(now);
    if let Some(s) = sun
        && let (Some(rise), Some(set)) = (s.next_rising, s.next_setting)
        && (now - 300..=now + SUN_HORIZON).contains(&rise)
        && (now - 300..=now + SUN_HORIZON).contains(&set)
    {
        return !s.up && local(rise).1 > today;
    }
    minutes(&hm).is_some_and(|m| m >= 18 * 60)
}

/// Rain, snow, hail and storms: the weather one plans for.
fn wet(condition: &str) -> bool {
    matches!(
        condition,
        "rainy" | "pouring" | "snowy" | "snowy-rainy" | "hail" | "lightning" | "lightning-rainy"
    )
}

/// How long ahead the sentence looks for a change in the hourly forecast.
const CHANGE_AHEAD: i64 = 8 * 3600;

/// The widget's big block. During the day: the condition now in words and, when the hourly
/// forecast turns from dry to wet or back in the next hours, from when ("Nuvoloso, pioggia dalle
/// 17:00"), with today's range under it. In the evening ([`evening`]), when the daily forecast
/// has tomorrow with its high: tomorrow's condition ("Domani pioggia") and its range. `today` is
/// the local date ("2026-11-08"), `now` seconds since the epoch, `local` writes a time as
/// ("15:30", local day number).
pub fn hero(
    w: &Weather,
    l: &Locale,
    today: &str,
    now: i64,
    local: &dyn Fn(i64) -> (String, i64),
) -> WeatherHero {
    let whole = |v: f64| format!("{}°", v.round() as i64);
    let words = |c: &str| {
        let key = condition_key(c);
        match l.text(&key) {
            k if k == key => l.text("weather_exceptional").to_string(),
            s => s.to_string(),
        }
    };
    // The condition inside a sentence: lower case, except where the language says not to
    // (German nouns).
    let inner = |c: &str| {
        let s = words(c);
        if l.text("weather_lowercase") == "no" {
            return s;
        }
        let mut it = s.chars();
        it.next()
            .map(|f| f.to_lowercase().chain(it).collect())
            .unwrap_or_default()
    };
    let range = |key: &str, low: Option<f64>, high: Option<f64>| match (low, high) {
        (Some(lo), Some(hi)) => l
            .text(key)
            .replace("{low}", &whole(lo))
            .replace("{high}", &whole(hi)),
        (None, Some(hi)) => l.text("weather_high").replace("{t}", &whole(hi)),
        (Some(lo), None) => l.text("weather_low").replace("{t}", &whole(lo)),
        (None, None) => String::new(),
    };
    let next_day = parse_date(today).map(|(d, _)| d + 1);
    let tomorrow = evening(w.sun.as_ref(), now, local)
        .then(|| {
            w.today
                .iter()
                .chain(&w.next_days)
                .find(|d| parse_date(&d.date).map(|(n, _)| n) == next_day && d.high.is_some())
        })
        .flatten();
    if let Some(d) = tomorrow {
        return WeatherHero {
            tomorrow: true,
            label: l
                .text("weather_tomorrow_is")
                .replace("{next}", &inner(&d.condition)),
            kind: d.condition.clone(),
            temp: d.high.map(whole).unwrap_or_default(),
            range: range("weather_range", d.low, d.high),
        };
    }
    let change = w
        .hours
        .iter()
        .filter(|h| h.at > now && h.at <= now + CHANGE_AHEAD)
        .find(|h| wet(&h.condition) != wet(&w.condition));
    let label = match change {
        Some(h) => l
            .text("weather_then")
            .replace("{now}", &words(&w.condition))
            .replace("{next}", &inner(&h.condition))
            .replace("{time}", &local(h.at).0),
        None => words(&w.condition),
    };
    let day = w.today.as_ref();
    WeatherHero {
        tomorrow: false,
        label,
        kind: w.condition.clone(),
        temp: whole(w.temperature),
        range: range(
            "weather_today_range",
            day.and_then(|d| d.low),
            day.and_then(|d| d.high),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ha::weather::icons;

    fn locale(code: &str) -> Locale {
        locale::load(&locale::repo_locales(), code).unwrap()
    }

    #[test]
    fn numbers() {
        assert_eq!(number(13.7, "."), "13.7");
        assert_eq!(number(13.7, ","), "13,7");
        assert_eq!(number(14.0, ","), "14");
        assert_eq!(number(13.96, "."), "14");
        assert_eq!(number(-0.04, "."), "0");
        assert_eq!(number(-2.5, ","), "-2,5");
    }

    #[test]
    fn sample_in_english_and_italian() {
        let w = Weather::sample();
        let en = text(&w, &locale("en"));
        assert_eq!(
            en,
            WeatherText {
                icon: icons::CLOUDY,
                condition: "Cloudy".into(),
                temperature: "13.7°C".into(),
                rounded: "14°".into(),
                high: "15°".into(),
                low: "8°".into(),
            }
        );
        let it = text(&w, &locale("it"));
        assert_eq!(it.temperature, "13,7°C");
        assert_eq!(it.condition, "Nuvoloso");
    }

    #[test]
    fn every_condition_has_words_in_every_language() {
        let conditions = [
            "sunny",
            "clear-night",
            "partlycloudy",
            "cloudy",
            "rainy",
            "pouring",
            "snowy",
            "snowy-rainy",
            "fog",
            "hail",
            "lightning",
            "lightning-rainy",
            "windy",
            "windy-variant",
            "exceptional",
        ];
        for code in locale::available(&locale::repo_locales()) {
            let l = locale(&code);
            for c in conditions {
                let key = condition_key(c);
                assert_ne!(l.text(&key), key, "{code}: {key}");
            }
        }
    }

    #[test]
    fn no_forecast_no_range() {
        let mut w = Weather::sample();
        w.today = None;
        w.condition = "something-new".into();
        let t = text(&w, &locale("en"));
        assert_eq!((t.high.as_str(), t.low.as_str()), ("", ""));
        assert_eq!(t.icon, icons::EXCEPTIONAL);
        assert_eq!(t.condition, "Exceptional");
    }

    #[test]
    fn forecast_rows_with_day_names_and_rain() {
        use crate::ha::weather::ForecastDay;
        let day = |date: &str, high, rain: Option<f64>, prob: Option<f64>| ForecastDay {
            date: date.into(),
            condition: "rainy".into(),
            high: Some(high),
            low: Some(4.0),
            precipitation: rain,
            precipitation_probability: prob,
            ..Default::default()
        };
        let mut w = Weather::sample();
        w.today = Some(day("2026-11-08", 15.0, None, None));
        w.next_days = vec![
            day("2026-11-09", 12.4, Some(2.5), None),
            day("2026-11-10", 11.0, Some(0.0), Some(60.0)),
            day("2026-11-11", 9.0, None, None),
        ];
        let en = forecast_rows(&w, &locale("en"), "2026-11-08", 3);
        assert_eq!(en.len(), 3);
        assert_eq!(en[0].day, "Today");
        assert_eq!(en[1].day, "Tomorrow");
        assert_eq!(en[1].rain, "2.5 mm");
        assert_eq!(en[1].high, "12°");
        assert_eq!(en[2].day, "Tuesday 10");
        assert_eq!(en[2].rain, "60%");
        let it = forecast_rows(&w, &locale("it"), "2026-11-08", 7);
        assert_eq!(it.len(), 4);
        assert_eq!(it[3].day, "Mercoledì 11");
        assert_eq!(it[1].rain, "2,5 mm");
        assert_eq!(it[3].rain, "");
        assert_eq!(it[0].condition, "Pioggia");
    }

    #[test]
    fn widget_columns_are_the_next_four_days() {
        let w = Weather::sample_week("2026-11-03");
        let hm = |t: i64| format!("{:02}:00", t.rem_euclid(86400) / 3600);
        let c = widget_columns(&w, &locale("it"), "2026-11-03", 0, &hm);
        let got: Vec<(&str, &str, &str)> = c
            .iter()
            .map(|r| (r.left.as_str(), r.note.as_str(), r.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("Mer 4", "rainy", "12°"),
                ("Gio 5", "sunny", "14°"),
                ("Ven 6", "partlycloudy", "13°"),
                ("Sab 7", "rainy", "11°"),
            ]
        );
        // Without a forecast: no columns (the page leaves the turn).
        assert!(widget_columns(&Weather::sample(), &locale("en"), "2026-11-03", 0, &hm).is_empty());
        // With the hours: the next four from the one going on (14:25 → from 14:00).
        let start = 20 * 86400 + 14 * 3600;
        let w = w.with_sample_hours(start - 3600);
        let c = widget_columns(&w, &locale("it"), "2026-11-03", start + 25 * 60, &hm);
        let got: Vec<(&str, &str, &str)> = c
            .iter()
            .map(|r| (r.left.as_str(), r.note.as_str(), r.text.as_str()))
            .collect();
        assert_eq!(
            got,
            [
                ("14:00", "partlycloudy", "14°"),
                ("15:00", "cloudy", "13°"),
                ("16:00", "rainy", "12°"),
                ("17:00", "rainy", "11°"),
            ]
        );
        // One hour left only: the days again.
        let late = widget_columns(&w, &locale("it"), "2026-11-03", start + 4 * 3600, &hm);
        assert_eq!(late[0].left, "Mer 4");
    }

    /// UTC as the local time, so nothing depends on the machine's zone or clock.
    fn utc(t: i64) -> (String, i64) {
        (
            format!(
                "{:02}:{:02}",
                t.rem_euclid(86400) / 3600,
                t.rem_euclid(3600) / 60
            ),
            t.div_euclid(86400),
        )
    }

    /// 2026-11-03 at `h`:`m` UTC.
    fn at(h: i64, m: i64) -> i64 {
        days_from_civil(2026, 11, 3) * 86400 + h * 3600 + m * 60
    }

    fn week() -> Weather {
        Weather::sample_week("2026-11-03")
    }

    #[test]
    fn evening_after_sunset_until_midnight() {
        let day = days_from_civil(2026, 11, 3);
        let sun = |now: i64| *week().with_sample_sun(day, now).sun.as_ref().unwrap();
        // Before dawn, at midnight and in the day: now (today's sunrise is still to come, or
        // the sun is up).
        for now in [
            at(0, 0),
            at(0, 1),
            at(5, 0),
            at(6, 58),
            at(12, 0),
            at(16, 51),
        ] {
            assert!(!evening(Some(&sun(now)), now, &utc), "{}", utc(now).0);
        }
        // From sunset to midnight: evening (the next sunrise is tomorrow).
        for now in [at(16, 52), at(17, 30), at(23, 59)] {
            assert!(evening(Some(&sun(now)), now, &utc), "{}", utc(now).0);
        }
        // Midnight passed: a new day, now again.
        let next = day * 86400 + 86400 + 30;
        assert!(!evening(Some(&sun(next)), next, &utc));
    }

    #[test]
    fn evening_by_the_clock_without_usable_sun() {
        // No sun.sun: from 18:00.
        assert!(!evening(None, at(17, 59), &utc));
        assert!(evening(None, at(18, 0), &utc));
        assert!(!evening(None, at(0, 10), &utc));
        // Polar day: up, the next sunset weeks away; polar night: down, the sunrise weeks away.
        let polar_day = Sun {
            up: true,
            next_setting: Some(at(12, 0) + 40 * 86400),
            next_rising: Some(at(12, 0) + 41 * 86400),
        };
        let polar_night = Sun {
            up: false,
            next_rising: Some(at(12, 0) + 30 * 86400),
            next_setting: Some(at(13, 0) + 30 * 86400),
        };
        for s in [polar_day, polar_night] {
            assert!(!evening(Some(&s), at(12, 0), &utc));
            assert!(evening(Some(&s), at(20, 0), &utc));
        }
        // Old times (Home Assistant has not moved them on): the clock too.
        let stale = Sun {
            up: false,
            next_rising: Some(at(7, 0) - 86400),
            next_setting: Some(at(16, 0) - 86400),
        };
        assert!(!evening(Some(&stale), at(17, 0), &utc));
        assert!(evening(Some(&stale), at(19, 0), &utc));
    }

    #[test]
    fn hero_now_in_the_day() {
        let now = at(14, 25);
        let w = week()
            .with_sample_sun(days_from_civil(2026, 11, 3), now)
            .with_sample_hours(at(13, 0));
        let h = hero(&w, &locale("it"), "2026-11-03", now, &utc);
        assert_eq!(
            h,
            WeatherHero {
                tomorrow: false,
                // Cloudy now, the hourly forecast turns to rain at 16:00.
                label: "Nuvoloso, pioggia dalle 16:00".into(),
                kind: "cloudy".into(),
                temp: "14°".into(),
                range: "Oggi tra 8° e 15°".into(),
            }
        );
        let en = hero(&w, &locale("en"), "2026-11-03", now, &utc);
        assert_eq!(en.label, "Cloudy, rainy from 16:00");
        assert_eq!(en.range, "Today 8° to 15°");
    }

    #[test]
    fn hero_says_a_change_only_when_dry_turns_wet_or_back() {
        let now = at(14, 25);
        let mut w = week().with_sample_hours(at(13, 0));
        // Dry all along (cloudy, partly cloudy): the condition alone.
        for h in &mut w.hours {
            if wet(&h.condition) {
                h.condition = "partlycloudy".into();
            }
        }
        assert_eq!(
            hero(&w, &locale("it"), "2026-11-03", now, &utc).label,
            "Nuvoloso"
        );
        // Raining now, dry from the next hour.
        w.condition = "rainy".into();
        let h = hero(&w, &locale("it"), "2026-11-03", now, &utc);
        assert_eq!(h.label, "Pioggia, nuvoloso dalle 15:00");
        // German keeps its nouns' capitals.
        w.hours.iter_mut().for_each(|h| h.condition = "fog".into());
        w.condition = "snowy".into();
        let de = hero(&w, &locale("de"), "2026-11-03", now, &utc);
        let fog = locale("de").text("weather_fog").to_string();
        assert!(de.label.contains(&format!(", {fog} ")), "{}", de.label);
        // A change more than 8 hours ahead is not said.
        let mut w = week().with_sample_hours(at(13, 0));
        w.hours.iter_mut().for_each(|h| h.at += 9 * 3600);
        assert_eq!(
            hero(&w, &locale("en"), "2026-11-03", now, &utc).label,
            "Cloudy"
        );
    }

    #[test]
    fn hero_tomorrow_in_the_evening() {
        let now = at(19, 10);
        let w = week().with_sample_sun(days_from_civil(2026, 11, 3), now);
        let h = hero(&w, &locale("it"), "2026-11-03", now, &utc);
        assert!(h.tomorrow);
        assert_eq!(h.label, "Domani pioggia");
        assert_eq!((h.kind.as_str(), h.temp.as_str()), ("rainy", "12°"));
        assert_eq!(h.range, "Tra 9° e 12°");
        let en = hero(&w, &locale("en"), "2026-11-03", now, &utc);
        assert_eq!(
            (en.label.as_str(), en.range.as_str()),
            ("Tomorrow: rainy", "9° to 12°")
        );
    }

    #[test]
    fn hero_falls_back_to_now() {
        // Evening, but no daily forecast: now, with no range.
        let mut w = Weather::sample();
        w.today = None;
        let h = hero(&w, &locale("en"), "2026-11-03", at(21, 0), &utc);
        assert!(!h.tomorrow);
        assert_eq!((h.temp.as_str(), h.range.as_str()), ("14°", ""));
        // Tomorrow's day without a high: now.
        let mut w = week();
        w.next_days[0].high = None;
        assert!(!hero(&w, &locale("en"), "2026-11-03", at(21, 0), &utc).tomorrow);
        // Only a high today: "high 15°".
        let mut w = week();
        w.today.as_mut().unwrap().low = None;
        assert_eq!(
            hero(&w, &locale("en"), "2026-11-03", at(10, 0), &utc).range,
            "high 15°"
        );
    }
}
