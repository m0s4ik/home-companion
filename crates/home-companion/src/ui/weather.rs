// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The weather as the home screen writes it (the date line of companion.slint):
//! icon from the condition, temperatures with the language's decimal separator, the condition
//! in words.

use locale::Locale;

use crate::ha::weather::{Weather, condition_icon};

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
}
