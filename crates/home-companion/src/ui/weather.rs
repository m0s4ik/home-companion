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
}
