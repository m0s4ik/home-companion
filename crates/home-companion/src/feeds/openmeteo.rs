// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Open-Meteo (no key; free for non-commercial use, CC BY 4.0: "Weather data by
//! Open-Meteo.com"): `v1/forecast` with every city's coordinates in one request gives the
//! weather now, the next hours and the next days, in the city's own time zone; the geocoding
//! API finds cities by name. The WMO weather codes become the condition words of Home
//! Assistant that the screen already has icons for.

use serde::{Deserialize, Serialize};

use super::http::{Host, Url};
use super::{CityChoice, Hit, clean_name};

/// The next hours and days kept.
pub const HOURS: usize = 8;
pub const DAYS: usize = 5;

/// A WMO weather code as Home Assistant's condition (`ui/weather.rs` draws those).
pub fn condition(code: i64, day: bool) -> &'static str {
    match code {
        0 => {
            if day {
                "sunny"
            } else {
                "clear-night"
            }
        }
        1 | 2 => "partlycloudy",
        3 => "cloudy",
        45 | 48 => "fog",
        51 | 53 | 55 | 61 | 63 | 80 | 81 => "rainy",
        65 | 82 => "pouring",
        56 | 57 | 66 | 67 => "snowy-rainy",
        71 | 73 | 75 | 77 | 85 | 86 => "snowy",
        95 => "lightning-rainy",
        96 | 99 => "hail",
        _ => "cloudy",
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hour {
    pub at: i64,
    pub temp: f32,
    pub cond: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Day {
    /// The day's start (unix seconds; local midnight there).
    pub at: i64,
    pub high: f32,
    pub low: f32,
    pub cond: String,
}

/// A city's weather.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CityWeather {
    /// [`CityChoice::id`].
    pub id: u64,
    pub temp: f32,
    pub cond: String,
    pub hours: Vec<Hour>,
    pub days: Vec<Day>,
    /// The city's offset from UTC now (seconds).
    pub utc_offset: i32,
    /// "°C" or "°F".
    pub unit: String,
    /// When it was fetched.
    pub at: i64,
}

pub fn forecast_url(cities: &[CityChoice], fahrenheit: bool) -> Url {
    let list = |f: &dyn Fn(&CityChoice) -> f64| {
        cities
            .iter()
            .map(|c| format!("{:.2}", f(c)))
            .collect::<Vec<_>>()
            .join(",")
    };
    let (lat, lon) = (list(&|c| c.lat), list(&|c| c.lon));
    Url::new(
        Host::OpenMeteo,
        "v1/forecast",
        &[
            ("latitude", &lat),
            ("longitude", &lon),
            ("current", "temperature_2m,weather_code,is_day"),
            ("hourly", "temperature_2m,weather_code,is_day"),
            (
                "daily",
                "weather_code,temperature_2m_max,temperature_2m_min",
            ),
            ("timezone", "auto"),
            ("forecast_days", "5"),
            ("forecast_hours", "8"),
            ("timeformat", "unixtime"),
            (
                "temperature_unit",
                if fahrenheit { "fahrenheit" } else { "celsius" },
            ),
        ],
    )
}

pub fn search_url(q: &str, lang: &str) -> Url {
    Url::new(
        Host::OpenMeteoGeo,
        "v1/search",
        &[("name", q), ("count", "6"), ("language", lang)],
    )
}

#[derive(Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    Many(Vec<Forecast>),
    One(Box<Forecast>),
}

#[derive(Deserialize)]
struct Forecast {
    #[serde(default)]
    utc_offset_seconds: i32,
    current: Option<Current>,
    #[serde(default)]
    current_units: Option<Units>,
    #[serde(default)]
    hourly: Option<Series>,
    #[serde(default)]
    daily: Option<Daily>,
}

#[derive(Deserialize)]
struct Units {
    #[serde(default)]
    temperature_2m: String,
}

#[derive(Deserialize)]
struct Current {
    temperature_2m: Option<f64>,
    weather_code: Option<i64>,
    #[serde(default)]
    is_day: Option<i64>,
}

#[derive(Deserialize)]
struct Series {
    #[serde(default)]
    time: Vec<i64>,
    #[serde(default)]
    temperature_2m: Vec<Option<f64>>,
    #[serde(default)]
    weather_code: Vec<Option<i64>>,
    #[serde(default)]
    is_day: Vec<Option<i64>>,
}

#[derive(Deserialize)]
struct Daily {
    #[serde(default)]
    time: Vec<i64>,
    #[serde(default)]
    weather_code: Vec<Option<i64>>,
    #[serde(default)]
    temperature_2m_max: Vec<Option<f64>>,
    #[serde(default)]
    temperature_2m_min: Vec<Option<f64>>,
}

/// A forecast answer for `cities` (in the request's order) → their weather; a city without a
/// temperature now is left out.
pub fn parse_forecast(
    body: &[u8],
    cities: &[CityChoice],
    now: i64,
) -> Result<Vec<CityWeather>, String> {
    let list =
        match serde_json::from_slice::<OneOrMany>(body).map_err(|e| format!("forecast: {e}"))? {
            OneOrMany::Many(v) => v,
            OneOrMany::One(f) => vec![*f],
        };
    if list.len() != cities.len() {
        return Err(format!(
            "forecast: {} answers for {} cities",
            list.len(),
            cities.len()
        ));
    }
    Ok(list
        .into_iter()
        .zip(cities)
        .filter_map(|(f, c)| {
            let cur = f.current?;
            let temp = cur.temperature_2m.filter(|t| super::sane_temp(*t))? as f32;
            let day = cur.is_day.unwrap_or(1) != 0;
            let hours = f
                .hourly
                .map(|h| {
                    (0..h.time.len())
                        .filter_map(|i| {
                            let t = (*h.temperature_2m.get(i)?)?;
                            let code = (*h.weather_code.get(i)?)?;
                            let day = h.is_day.get(i).copied().flatten().unwrap_or(1) != 0;
                            super::sane_temp(t).then(|| Hour {
                                at: h.time[i],
                                temp: t as f32,
                                cond: condition(code, day).into(),
                            })
                        })
                        .take(HOURS)
                        .collect()
                })
                .unwrap_or_default();
            let days = f
                .daily
                .map(|d| {
                    (0..d.time.len())
                        .filter_map(|i| {
                            let hi = (*d.temperature_2m_max.get(i)?)?;
                            let lo = (*d.temperature_2m_min.get(i)?)?;
                            let code = (*d.weather_code.get(i)?)?;
                            (super::sane_temp(hi) && super::sane_temp(lo)).then(|| Day {
                                at: d.time[i],
                                high: hi as f32,
                                low: lo as f32,
                                cond: condition(code, true).into(),
                            })
                        })
                        .take(DAYS)
                        .collect()
                })
                .unwrap_or_default();
            let unit = match f.current_units.map(|u| u.temperature_2m) {
                Some(u) if u.contains('F') => "°F",
                _ => "°C",
            };
            Some(CityWeather {
                id: c.id,
                temp,
                cond: condition(cur.weather_code.unwrap_or(3), day).into(),
                hours,
                days,
                utc_offset: f.utc_offset_seconds.clamp(-14 * 3600, 14 * 3600),
                unit: unit.into(),
                at: now,
            })
        })
        .collect())
}

#[derive(Deserialize)]
struct Geo {
    #[serde(default)]
    results: Vec<Place>,
}

#[derive(Deserialize)]
struct Place {
    id: u64,
    #[serde(default)]
    name: String,
    latitude: f64,
    longitude: f64,
    #[serde(default)]
    country_code: Option<String>,
    #[serde(default)]
    admin1: Option<String>,
}

/// A geocoding answer → at most six cities: the hit ("Springfield", "Illinois, US") and the
/// choice it becomes (coordinates rounded to two decimals).
pub fn parse_search(body: &[u8]) -> Result<Vec<(Hit, CityChoice)>, String> {
    let g: Geo = serde_json::from_slice(body).map_err(|e| format!("geocoding: {e}"))?;
    Ok(g.results
        .into_iter()
        .filter_map(|p| {
            let name = clean_name(&p.name);
            let region = clean_name(p.admin1.as_deref().unwrap_or(""));
            let country: String = p
                .country_code
                .unwrap_or_default()
                .chars()
                .filter(|c| c.is_ascii_alphabetic())
                .take(2)
                .collect::<String>()
                .to_ascii_uppercase();
            let round = |v: f64| (v * 100.0).round() / 100.0;
            let c = CityChoice {
                id: p.id,
                name: name.clone(),
                region: region.clone(),
                country: country.clone(),
                lat: round(p.latitude),
                lon: round(p.longitude),
            };
            if name.is_empty() || !c.valid() {
                return None;
            }
            let detail = [region, country]
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(", ");
            Some((
                Hit {
                    key: c.key(),
                    name,
                    detail,
                },
                c,
            ))
        })
        .take(6)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::tests::fixture;

    fn city(id: u64, lat: f64, lon: f64) -> CityChoice {
        CityChoice {
            id,
            name: format!("City {id}"),
            region: String::new(),
            country: String::new(),
            lat,
            lon,
        }
    }

    #[test]
    fn every_code_has_an_icon() {
        let icons = [
            "sunny",
            "clear-night",
            "partlycloudy",
            "cloudy",
            "fog",
            "rainy",
            "pouring",
            "snowy-rainy",
            "snowy",
            "lightning-rainy",
            "hail",
        ];
        for code in 0..=100 {
            for day in [true, false] {
                assert!(icons.contains(&condition(code, day)), "{code}");
            }
        }
        assert_eq!(condition(0, false), "clear-night");
        assert_eq!(condition(65, true), "pouring");
    }

    #[test]
    fn two_cities_in_one_answer() {
        let cities = [city(2950159, 52.52, 13.41), city(4250542, 39.80, -89.64)];
        let w = parse_forecast(&fixture("openmeteo-forecast-two.json"), &cities, 5).unwrap();
        assert_eq!(w.len(), 2);
        assert_eq!(w[0].id, 2950159);
        assert_eq!(w[0].utc_offset, 3600);
        assert_eq!(w[1].utc_offset, -21600);
        assert_eq!((w[0].temp, w[0].cond.as_str()), (2.4, "clear-night"));
        assert_eq!(w[1].cond, "sunny");
        assert_eq!(w[0].hours.len(), 8);
        assert_eq!(w[0].days.len(), 5);
        assert_eq!(w[0].days[0].cond, "rainy");
        assert_eq!(w[0].unit, "°C");
        assert_eq!(w[0].at, 5);
        // One city: an object, not a list; in Fahrenheit.
        let one =
            parse_forecast(&fixture("openmeteo-forecast-one-f.json"), &cities[..1], 5).unwrap();
        assert_eq!(one[0].unit, "°F");
        // As many answers as cities, or nothing.
        assert!(parse_forecast(&fixture("openmeteo-forecast-two.json"), &cities[..1], 5).is_err());
        for bad in [&b""[..], b"<html>", b"[{\"current\":", b"42"] {
            assert!(parse_forecast(bad, &cities[..1], 0).is_err());
        }
        // An absurd temperature is none.
        let w = parse_forecast(br#"{"current":{"temperature_2m":1e40}}"#, &cities[..1], 0).unwrap();
        assert!(w.is_empty());
        // No temperature now: that city is left out.
        let w = parse_forecast(br#"{"current":{"temperature_2m":null}}"#, &cities[..1], 0).unwrap();
        assert!(w.is_empty());
    }

    #[test]
    fn cities_found_by_name() {
        let h = parse_search(&fixture("openmeteo-geocoding-springfield.json")).unwrap();
        assert_eq!(h.len(), 6);
        let (hit, c) = &h[1];
        assert_eq!(hit.key, "city:4250542");
        assert_eq!(
            (hit.name.as_str(), hit.detail.as_str()),
            ("Springfield", "Illinois, US")
        );
        assert_eq!((c.lat, c.lon), (39.8, -89.64));
        assert!(
            parse_search(&fixture("openmeteo-geocoding-none.json"))
                .unwrap()
                .is_empty()
        );
        let bad = parse_search(br#"{"results":[{"id":1,"name":"X","latitude":95,"longitude":0}]}"#)
            .unwrap();
        assert!(bad.is_empty());
    }

    #[test]
    fn the_url_carries_every_city() {
        let u = forecast_url(&[city(1, 52.52, 13.41), city(2, -33.87, 151.21)], true);
        assert!(u.as_str().starts_with("https://api.open-meteo.com/v1/forecast?latitude=52.52%2C-33.87&longitude=13.41%2C151.21&"));
        assert!(u.as_str().ends_with("temperature_unit=fahrenheit"));
        assert_eq!(
            search_url("São Paulo", "it").as_str(),
            "https://geocoding-api.open-meteo.com/v1/search?name=S%C3%A3o%20Paulo&count=6&language=it"
        );
    }
}
