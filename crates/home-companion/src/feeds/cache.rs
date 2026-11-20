// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! `feeds.json` next to settings.toml: the feeds' last data, read at start so the widget shows
//! it at once (with "data from …" when old), written whole (a temporary file renamed, mode 600)
//! and only when it changed: about 150 writes a day on the device's flash at most.

use std::path::Path;

use super::Snapshot;

/// The cache, or nothing (missing, unreadable, from another version: the feeds fetch again).
pub fn read(path: &Path) -> Snapshot {
    let Ok(text) = std::fs::read(path) else {
        return Snapshot::default();
    };
    if text.len() > 1024 * 1024 {
        return Snapshot::default();
    }
    serde_json::from_slice(&text).unwrap_or_default()
}

/// Writes the cache if it differs from what is on disk. A value that is not a number would be
/// written as null and spoil the whole file: such a quote or city is left out.
pub fn write(path: &Path, s: &Snapshot) {
    let mut s = s.clone();
    s.quotes.retain(|q| {
        [q.price, q.low, q.high].iter().all(|v| v.is_finite())
            && q.prev.is_none_or(f64::is_finite)
            && q.points.iter().all(|p| p.is_finite())
    });
    s.cities.retain(|c| {
        c.temp.is_finite()
            && c.hours.iter().all(|h| h.temp.is_finite())
            && c.days
                .iter()
                .all(|d| d.high.is_finite() && d.low.is_finite())
    });
    let Ok(text) = serde_json::to_vec(&s) else {
        return;
    };
    if std::fs::read(path).is_ok_and(|old| old == text) {
        return;
    }
    let tmp = path.with_extension("json.new");
    let result = (|| -> std::io::Result<()> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(&text)?;
        std::fs::rename(&tmp, path)
    })();
    if let Err(e) = result {
        log::warn!(event = "feeds"; "feeds: {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::Quote;

    #[test]
    fn written_once_and_read_back() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("hc-feeds-cache-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("feeds.json");
        assert_eq!(read(&path), Snapshot::default());
        let s = Snapshot {
            quotes: vec![Quote {
                key: "yahoo:AAPL".into(),
                price: 1.5,
                prev: None,
                unit: "USD".into(),
                decimals: 2,
                points: vec![1.0, 2.0],
                low: 1.0,
                high: 2.0,
                market_open: false,
                at: 7,
            }],
            ..Default::default()
        };
        write(&path, &s);
        assert_eq!(read(&path), s);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // The same again: not rewritten.
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write(&path, &s);
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        // Broken: nothing.
        std::fs::write(&path, b"{\"quotes\":[{").unwrap();
        assert_eq!(read(&path), Snapshot::default());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
