// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! CoinGecko's public API (no key; about ten requests a minute): `coins/markets` gives every
//! chosen coin's price, its change over 24 hours and, with `sparkline=true`, the last seven
//! days hour by hour, of which the last 24 hours are the curve: one request for all the coins,
//! every ten minutes. `search` finds coins by name. "Crypto data by CoinGecko".

use serde::Deserialize;

use super::http::{Host, Url};
use super::{Hit, Quote, clean_name, valid_coin};
use crate::ui::widgets::{CURVE_POINTS, downsample};

/// The currencies the coins can be shown in (one for all, chosen in the panel).
pub const CURRENCIES: [&str; 7] = ["usd", "eur", "gbp", "chf", "jpy", "cad", "aud"];

pub fn valid_currency(c: &str) -> bool {
    CURRENCIES.contains(&c)
}

pub fn markets_url(ids: &[&str], currency: &str) -> Url {
    Url::new(
        Host::CoinGecko,
        "api/v3/coins/markets",
        &[
            ("vs_currency", currency),
            ("ids", &ids.join(",")),
            ("sparkline", "true"),
        ],
    )
}

pub fn search_url(q: &str) -> Url {
    Url::new(Host::CoinGecko, "api/v3/search", &[("query", q)])
}

#[derive(Deserialize)]
struct Market {
    id: String,
    current_price: Option<f64>,
    #[serde(default)]
    price_change_24h: Option<f64>,
    #[serde(default)]
    sparkline_in_7d: Option<Sparkline>,
}

#[derive(Deserialize)]
struct Sparkline {
    #[serde(default)]
    price: Vec<Option<f64>>,
}

/// Digits after the separator for a price of this size.
fn decimals(price: f64) -> u8 {
    match price.abs() {
        p if p >= 1000.0 => 0,
        p if p >= 1.0 => 2,
        p if p >= 0.01 => 4,
        _ => 6,
    }
}

/// A markets answer → a quote per coin asked (`ids`) with a price (`coingecko:<id>`), in
/// `currency`.
pub fn parse_markets(
    body: &[u8],
    ids: &[&str],
    currency: &str,
    now: i64,
) -> Result<Vec<Quote>, String> {
    let list: Vec<Market> = serde_json::from_slice(body).map_err(|e| format!("markets: {e}"))?;
    Ok(list
        .into_iter()
        .filter(|m| valid_coin(&m.id) && ids.contains(&m.id.as_str()))
        .filter_map(|m| {
            let price = m.current_price.filter(|p| super::sane(*p))?;
            let prev = m
                .price_change_24h
                .map(|d| price - d)
                .filter(|p| super::sane(*p));
            let hours: Vec<f64> = m
                .sparkline_in_7d
                .map(|s| {
                    s.price
                        .into_iter()
                        .map(|p| p.filter(|p| super::sane(*p)).unwrap_or(f64::NAN))
                        .collect()
                })
                .unwrap_or_default();
            // The hourly points of the last day (and the price now, its end). CoinGecko gives the
            // curve in US dollars whatever `vs_currency` is: in another currency the curve is
            // brought to the price's, scaled so that its last point meets the price now.
            let mut day: Vec<f64> = hours[hours.len().saturating_sub(24)..].to_vec();
            if !currency.eq_ignore_ascii_case("usd") {
                let last = day.iter().rev().copied().find(|p| p.is_finite());
                match last {
                    Some(last) if last > 0.0 => day.iter_mut().for_each(|p| *p *= price / last),
                    _ => day.clear(),
                }
            }
            if !day.is_empty() {
                day.push(price);
            }
            let good = day.iter().copied().filter(|p| p.is_finite());
            let (low, high) = good.fold((price, price), |(l, h), p| (l.min(p), h.max(p)));
            Some(Quote {
                key: format!("coingecko:{}", m.id),
                price,
                prev,
                unit: currency.to_ascii_uppercase(),
                decimals: decimals(price),
                points: downsample(&day, CURVE_POINTS),
                low,
                high,
                market_open: true,
                at: now,
            })
        })
        .collect())
}

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    coins: Vec<Coin>,
}

#[derive(Deserialize)]
struct Coin {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    symbol: String,
    #[serde(default)]
    market_cap_rank: Option<u32>,
}

/// A search answer → at most three coins, the biggest first among the first six found.
pub fn parse_search(body: &[u8], crypto: &str) -> Result<Vec<Hit>, String> {
    let s: Search = serde_json::from_slice(body).map_err(|e| format!("search: {e}"))?;
    let mut coins: Vec<Coin> = s
        .coins
        .into_iter()
        .filter(|c| valid_coin(&c.id))
        .take(6)
        .collect();
    coins.sort_by_key(|c| c.market_cap_rank.unwrap_or(u32::MAX));
    Ok(coins
        .into_iter()
        .take(3)
        .map(|c| {
            let name = clean_name(&c.name);
            let symbol: String = clean_name(&c.symbol).chars().take(12).collect();
            Hit {
                key: format!("coingecko:{}", c.id),
                name: if name.is_empty() { c.id.clone() } else { name },
                detail: if symbol.is_empty() {
                    crypto.to_string()
                } else {
                    format!("{symbol} · {crypto}")
                },
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::tests::fixture;

    #[test]
    fn markets_with_the_days_curve() {
        // Only what was asked.
        let one = parse_markets(
            &fixture("coingecko-markets-eur.json"),
            &["ethereum"],
            "eur",
            7,
        )
        .unwrap();
        assert_eq!(one.len(), 1);
        let q = parse_markets(
            &fixture("coingecko-markets-eur.json"),
            &["bitcoin", "ethereum"],
            "eur",
            7,
        )
        .unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q[0].key, "coingecko:bitcoin");
        assert_eq!(q[0].price, 53704.0);
        assert_eq!((q[0].unit.as_str(), q[0].decimals), ("EUR", 0));
        assert!(q[0].prev.unwrap() > q[0].price, "it fell that day");
        assert!(q[0].points.len() == 25);
        assert!(q[0].low <= q[0].price && q[0].high > q[0].low);
        // The curve, in dollars from CoinGecko, is brought to euros: no fall at its end.
        let n = q[0].points.len();
        let (before, end) = (q[0].points[n - 2] as f64, q[0].points[n - 1] as f64);
        assert!((end - q[0].price).abs() < 1.0, "{end}");
        assert!((before - end).abs() / end < 0.01, "{before} → {end}");
        assert!(q[0].high < 60_000.0, "{}", q[0].high);
        assert_eq!(q[0].at, 7);
        assert!(q[0].market_open);
    }

    #[test]
    fn broken_markets() {
        for bad in [&b""[..], b"{}", b"<html>", b"[{\"id\":\"x\"", b"null"] {
            assert!(
                parse_markets(bad, &["a"], "usd", 0).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        let q = parse_markets(
            br#"[{"id":"a","current_price":0},{"id":"Bad Id","current_price":1},
                {"id":"b","current_price":0.004,"sparkline_in_7d":{"price":[null,0.003]}},
                {"id":"c","current_price":null},{"id":"d","current_price":1e300},
                {"id":"e","current_price":5}]"#,
            &["a", "Bad Id", "b", "c", "d"],
            "usd",
            0,
        )
        .unwrap();
        assert_eq!(q.len(), 1);
        assert_eq!((q[0].decimals, q[0].prev), (6, None));
        assert_eq!(q[0].points, vec![0.003, 0.004]);
    }

    #[test]
    fn search_takes_the_biggest_three() {
        let h = parse_search(&fixture("coingecko-search-bitcoin.json"), "Crypto").unwrap();
        assert_eq!(h.len(), 3);
        assert_eq!(h[0].key, "coingecko:bitcoin");
        assert_eq!(h[0].detail, "BTC · Crypto");
        assert!(parse_search(b"\"x\"", "Crypto").is_err());
        assert!(parse_search(b"{}", "Crypto").unwrap().is_empty());
    }

    #[test]
    fn urls() {
        assert_eq!(
            markets_url(&["bitcoin", "ethereum"], "eur").as_str(),
            "https://api.coingecko.com/api/v3/coins/markets?vs_currency=eur&ids=bitcoin%2Cethereum&sparkline=true"
        );
        assert!(valid_currency("chf") && !valid_currency("btc") && !valid_currency("USD"));
    }
}
