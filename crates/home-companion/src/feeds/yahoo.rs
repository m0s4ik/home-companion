// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Yahoo Finance (unofficial: it may change or stop): `v8/finance/chart/<symbol>?range=1d&
//! interval=15m` gives the price, yesterday's close, the currency, the day's curve and the
//! market's hours in one answer; `v1/finance/search` finds symbols by name. Only the fields we
//! use are read; an answer without a price is an error, never half a quote.

use serde::Deserialize;

use super::http::{Host, Url, path_segment};
use super::{Hit, Quote, clean_name};
use crate::ui::widgets::{CURVE_POINTS, downsample};

/// A symbol as Yahoo writes them ("AAPL", "^GSPC", "EURUSD=X", "BRK-B", "ENI.MI").
pub fn valid_symbol(s: &str) -> bool {
    (1..=24).contains(&s.len())
        && s.as_bytes()[0] != b'-'
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'^' | b'='))
}

pub fn chart_url(host: Host, symbol: &str) -> Url {
    Url::new(
        host,
        &format!("v8/finance/chart/{}", path_segment(symbol)),
        &[("range", "1d"), ("interval", "15m")],
    )
}

pub fn search_url(host: Host, q: &str) -> Url {
    Url::new(
        host,
        "v1/finance/search",
        &[("q", q), ("quotesCount", "6"), ("newsCount", "0")],
    )
}

#[derive(Deserialize)]
struct Chart {
    chart: ChartBody,
}

#[derive(Deserialize)]
struct ChartBody {
    result: Option<Vec<ChartResult>>,
}

#[derive(Deserialize)]
struct ChartResult {
    meta: Meta,
    #[serde(default)]
    indicators: Option<Indicators>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Meta {
    #[serde(default)]
    currency: Option<String>,
    #[serde(default)]
    instrument_type: Option<String>,
    regular_market_price: Option<f64>,
    #[serde(default)]
    previous_close: Option<f64>,
    #[serde(default)]
    chart_previous_close: Option<f64>,
    #[serde(default)]
    price_hint: Option<u8>,
    #[serde(default)]
    current_trading_period: Option<Periods>,
}

#[derive(Deserialize)]
struct Periods {
    regular: Option<Period>,
}

#[derive(Deserialize)]
struct Period {
    start: i64,
    end: i64,
}

#[derive(Deserialize)]
struct Indicators {
    #[serde(default)]
    quote: Vec<Series>,
}

#[derive(Deserialize)]
struct Series {
    #[serde(default)]
    close: Vec<Option<f64>>,
}

/// A chart answer → the quote (its `key` and `at` as given). `now` decides whether the market
/// is open (its regular hours today).
pub fn parse_chart(body: &[u8], key: &str, now: i64) -> Result<Quote, String> {
    let c: Chart = serde_json::from_slice(body).map_err(|e| format!("chart: {e}"))?;
    let r = c
        .chart
        .result
        .and_then(|mut r| (!r.is_empty()).then(|| r.swap_remove(0)))
        .ok_or("chart: no result")?;
    let m = r.meta;
    let price = m
        .regular_market_price
        .filter(|p| super::sane(*p))
        .ok_or("chart: no price")?;
    let prev = m
        .previous_close
        .or(m.chart_previous_close)
        .filter(|p| super::sane(*p));
    let closes: Vec<f64> = r
        .indicators
        .map(|i| {
            i.quote
                .into_iter()
                .next()
                .map(|s| {
                    s.close
                        .into_iter()
                        .map(|c| c.filter(|c| super::sane(*c)).unwrap_or(f64::NAN))
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default();
    let good: Vec<f64> = closes.iter().copied().filter(|c| c.is_finite()).collect();
    let low = good.iter().copied().fold(price, f64::min);
    let high = good.iter().copied().fold(price, f64::max);
    // An index's points and a currency pair's rate have no currency sign.
    let unit = match m.instrument_type.as_deref() {
        Some("INDEX") | Some("CURRENCY") => String::new(),
        _ => m.currency.unwrap_or_default(),
    };
    let unit: String = unit
        .chars()
        .filter(|c| c.is_ascii_alphabetic())
        .take(4)
        .collect();
    let market_open = m
        .current_trading_period
        .and_then(|p| p.regular)
        .is_some_and(|p| (p.start..p.end).contains(&now));
    Ok(Quote {
        key: key.to_string(),
        price,
        prev,
        unit,
        decimals: m.price_hint.unwrap_or(2).min(6),
        points: downsample(&closes, CURVE_POINTS),
        low,
        high,
        market_open,
        at: now,
    })
}

/// Several symbols' day in one request (a search's hits, so each shows the row it will become).
pub fn spark_url(host: Host, symbols: &[&str]) -> Url {
    Url::new(
        host,
        "v8/finance/spark",
        &[
            ("symbols", &symbols.join(",")),
            ("range", "1d"),
            ("interval", "15m"),
        ],
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Spark {
    #[serde(default)]
    close: Vec<Option<f64>>,
    #[serde(default)]
    previous_close: Option<f64>,
    #[serde(default)]
    chart_previous_close: Option<f64>,
}

/// A spark answer → a quote per symbol asked that has a price (`yahoo:<symbol>`, the last close).
/// Spark says no currency: the quotes have none (the row it becomes shows the number alone).
pub fn parse_spark(body: &[u8], symbols: &[&str], now: i64) -> Result<Vec<Quote>, String> {
    let map: std::collections::HashMap<String, Spark> =
        serde_json::from_slice(body).map_err(|e| format!("spark: {e}"))?;
    Ok(symbols
        .iter()
        .filter_map(|sym| {
            let s = map.get(*sym)?;
            let closes: Vec<f64> = s
                .close
                .iter()
                .map(|c| c.filter(|c| super::sane(*c)).unwrap_or(f64::NAN))
                .collect();
            let price = closes.iter().rev().copied().find(|c| c.is_finite())?;
            let good = closes.iter().copied().filter(|c| c.is_finite());
            let (low, high) = good.fold((price, price), |(l, h), p| (l.min(p), h.max(p)));
            let decimals = if price < 10.0 { 4 } else { 2 };
            Some(Quote {
                key: format!("yahoo:{sym}"),
                price,
                prev: s
                    .previous_close
                    .or(s.chart_previous_close)
                    .filter(|p| super::sane(*p)),
                unit: String::new(),
                decimals,
                points: downsample(&closes, CURVE_POINTS),
                low,
                high,
                market_open: false,
                at: now,
            })
        })
        .collect())
}

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    quotes: Vec<SearchQuote>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SearchQuote {
    symbol: Option<String>,
    #[serde(default)]
    shortname: Option<String>,
    #[serde(default)]
    longname: Option<String>,
    #[serde(default)]
    exch_disp: Option<String>,
    #[serde(default)]
    type_disp: Option<String>,
    #[serde(default)]
    quote_type: Option<String>,
}

/// The kinds of things the markets page shows (futures and options left out: too many, too
/// short-lived).
const KINDS: &[&str] = &["EQUITY", "ETF", "INDEX", "CURRENCY", "MUTUALFUND"];

/// A search answer → at most six hits: `yahoo:<symbol>`, the name, "NASDAQ · Equity".
pub fn parse_search(body: &[u8]) -> Result<Vec<Hit>, String> {
    let s: Search = serde_json::from_slice(body).map_err(|e| format!("search: {e}"))?;
    Ok(s.quotes
        .into_iter()
        .filter(|q| KINDS.contains(&q.quote_type.as_deref().unwrap_or("")))
        .filter_map(|q| {
            let sym = q.symbol.filter(|s| valid_symbol(s))?;
            let name = clean_name(
                q.longname
                    .as_deref()
                    .or(q.shortname.as_deref())
                    .unwrap_or(&sym),
            );
            let detail = [q.exch_disp, q.type_disp]
                .into_iter()
                .flatten()
                .map(|d| clean_name(&d))
                .filter(|d| !d.is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            Some(Hit {
                key: format!("yahoo:{sym}"),
                name: if name.is_empty() { sym.clone() } else { name },
                detail: format!("{sym} · {detail}"),
            })
        })
        .take(6)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spark_of_three_symbols() {
        let body = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/feeds/yahoo-spark-three.json"),
        )
        .unwrap();
        let q = parse_spark(&body, &["AAPL", "^GSPC", "EURUSD=X", "NOPE.XX"], 7).unwrap();
        let keys: Vec<&str> = q.iter().map(|q| q.key.as_str()).collect();
        assert_eq!(keys, ["yahoo:AAPL", "yahoo:^GSPC", "yahoo:EURUSD=X"]);
        assert_eq!(q[0].price, 342.56);
        assert_eq!(q[0].prev, Some(349.21));
        assert!(q[0].points.len() > 10 && q[0].unit.is_empty() && q[0].at == 7);
        assert_eq!(q[2].decimals, 4);
        assert!(parse_spark(b"[]", &["AAPL"], 0).is_err());
        let u = spark_url(Host::Yahoo, &["AAPL", "^GSPC"])
            .as_str()
            .to_string();
        assert!(u.contains("v8/finance/spark?symbols=AAPL%2C%5EGSPC"), "{u}");
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures/feeds")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn a_stock_an_index_and_a_currency() {
        // The answers were recorded on a weekday afternoon in New York.
        let now = 1_795_198_800;
        let q = parse_chart(&fixture("yahoo-chart-aapl.json"), "yahoo:AAPL", now).unwrap();
        assert_eq!(q.price, 345.49);
        assert_eq!(q.prev, Some(349.21));
        assert_eq!((q.unit.as_str(), q.decimals), ("USD", 2));
        assert!(q.market_open);
        assert!(q.points.len() >= 10 && q.points.len() <= CURVE_POINTS);
        assert!(q.low <= q.price && q.high >= q.price && q.low < q.high);
        assert!(
            !parse_chart(&fixture("yahoo-chart-aapl.json"), "k", now + 86400)
                .unwrap()
                .market_open
        );
        let i = parse_chart(&fixture("yahoo-chart-gspc.json"), "yahoo:^GSPC", now).unwrap();
        assert_eq!(i.unit, "");
        let c = parse_chart(&fixture("yahoo-chart-eurusd.json"), "yahoo:EURUSD=X", now).unwrap();
        assert_eq!((c.unit.as_str(), c.decimals), ("", 4));
        assert!(c.price > 0.5 && c.price < 2.0);
    }

    #[test]
    fn broken_answers_are_errors() {
        let now = 0;
        for bad in [
            &fixture("yahoo-chart-notfound.json")[..],
            b"",
            b"<html>blocked</html>",
            b"{\"chart\":{\"result\":[]}}",
            b"{\"chart\":{\"result\":[{\"meta\":{\"regularMarketPrice\":0}}]}}",
            b"{\"chart\":{\"result\":[{\"meta\":{\"regularMarketPrice\":null}}]}}",
            br#"{"chart":{"result":[{"meta":{"currency":"USD""#,
        ] {
            assert!(
                parse_chart(bad, "k", now).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
        // Gaps in the curve (null) are skipped; no previous close is no change.
        let q = parse_chart(
            br#"{"chart":{"result":[{"meta":{"regularMarketPrice":10.5,"currency":"EUR"},
                "indicators":{"quote":[{"close":[10.0,null,11.0,null]}]}}]}}"#,
            "k",
            now,
        )
        .unwrap();
        assert_eq!(q.points, vec![10.0, 11.0]);
        assert_eq!((q.prev, q.low, q.high), (None, 10.0, 11.0));
        assert_eq!(q.unit, "EUR");
        // A currency that is not one.
        let q = parse_chart(
            br#"{"chart":{"result":[{"meta":{"regularMarketPrice":1,"currency":"<b>\u0007USDX"}}]}}"#,
            "k",
            now,
        )
        .unwrap();
        assert_eq!(q.unit, "bUSD");
    }

    #[test]
    fn search_hits() {
        let h = parse_search(&fixture("yahoo-search-apple.json")).unwrap();
        // The futures are left out.
        assert!(h.iter().all(|x| !x.key.ends_with("=F")));
        assert_eq!(h[0].key, "yahoo:AAPL");
        assert_eq!(h[0].name, "Apple Inc.");
        assert_eq!(h[0].detail, "AAPL · NASDAQ · Equity");
        assert!(h.len() <= 6);
        assert!(parse_search(b"{}").unwrap().is_empty());
        assert!(parse_search(b"nope").is_err());
        let evil = parse_search(
            br#"{"quotes":[{"symbol":"../x","quoteType":"EQUITY"},
            {"symbol":"OK","longname":"<img src=x onerror=1>\u0000\n Co","quoteType":"EQUITY"}]}"#,
        )
        .unwrap();
        assert_eq!(evil.len(), 1);
        assert_eq!(evil[0].name, "<img src=x onerror=1> Co");
    }

    #[test]
    fn symbols() {
        for ok in ["AAPL", "^GSPC", "EURUSD=X", "BRK-B", "ENI.MI"] {
            assert!(valid_symbol(ok), "{ok}");
        }
        let long = "x".repeat(25);
        for bad in ["", "A/B", "a b", "-o", long.as_str(), "../x"] {
            assert!(!valid_symbol(bad), "{bad}");
        }
        // Even so a symbol is only ever a path segment in --url.
        assert!(
            chart_url(Host::Yahoo, "-o")
                .as_str()
                .starts_with("https://query1.finance.yahoo.com/v8/finance/chart/-o?")
        );
        assert_eq!(
            chart_url(Host::Yahoo2, "^GSPC").as_str(),
            "https://query2.finance.yahoo.com/v8/finance/chart/%5EGSPC?range=1d&interval=15m"
        );
    }
}
