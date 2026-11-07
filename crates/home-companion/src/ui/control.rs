// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The control center (pulled down from the top of the desk): one tile per thing
//! that can be on or off, what it is doing in one short line. Pure: the screen asks for the tiles
//! with the current state and the words of the locale.

/// One tile as the screen draws it (`ControlRow` in companion.slint).
#[derive(Debug, Clone, PartialEq)]
pub struct ControlTileView {
    /// Picks the icon: "wifi", "bluetooth", "airplay", "mic", "night", "ha".
    pub key: &'static str,
    pub label: String,
    pub status: String,
    pub on: bool,
    /// False: shown greyed, not touchable (no Bluetooth adapter yet).
    pub enabled: bool,
    /// False: it only tells (Wi-Fi: switching it off here would cut Home Assistant).
    pub tappable: bool,
}

/// What the tiles show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ControlState {
    /// The Wi-Fi network joined (None: not connected).
    pub wifi: Option<String>,
    /// None: no Bluetooth on this device (yet); Some(on).
    pub bluetooth: Option<bool>,
    /// None: AirPlay off; Some(""): on and waiting; Some(sender): playing from it.
    pub airplay: Option<String>,
    pub mic_on: bool,
    /// Night now, and the hour it starts ("23:00").
    pub night_now: bool,
    pub night_from: String,
    /// None: not linked; Some(ok).
    pub ha: Option<bool>,
}

pub fn tiles(s: &ControlState, w: &dyn Fn(&str) -> String) -> Vec<ControlTileView> {
    let tile = |key, label: &str, status: String, on, enabled| ControlTileView {
        key,
        label: w(label),
        status,
        on,
        enabled,
        tappable: key != "wifi",
    };
    vec![
        tile(
            "wifi",
            "cc_wifi",
            s.wifi.clone().unwrap_or_else(|| w("cc_not_connected")),
            s.wifi.is_some(),
            true,
        ),
        match s.bluetooth {
            None => tile(
                "bluetooth",
                "cc_bluetooth",
                w("cc_unavailable"),
                false,
                false,
            ),
            Some(on) => tile(
                "bluetooth",
                "cc_bluetooth",
                w(if on { "cc_on" } else { "cc_off" }),
                on,
                true,
            ),
        },
        match &s.airplay {
            None => tile("airplay", "cc_airplay", w("cc_off"), false, true),
            Some(src) if src.is_empty() => tile("airplay", "cc_airplay", w("cc_ready"), true, true),
            Some(src) => tile("airplay", "cc_airplay", src.clone(), true, true),
        },
        tile(
            "mic",
            "cc_mic",
            w(if s.mic_on { "cc_listening" } else { "cc_off" }),
            s.mic_on,
            true,
        ),
        tile(
            "night",
            "cc_night",
            if s.night_now {
                w("cc_on")
            } else {
                w("cc_night_from").replace("{from}", &s.night_from)
            },
            s.night_now,
            true,
        ),
        match s.ha {
            None => tile("ha", "ha_short", w("cc_not_linked"), false, true),
            Some(true) => tile("ha", "ha_short", w("cc_linked"), true, true),
            Some(false) => tile("ha", "ha_short", w("cc_check"), false, true),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(k: &str) -> String {
        match k {
            "cc_off" => "Spento",
            "cc_on" => "Acceso",
            "cc_ready" => "Pronto",
            "cc_unavailable" => "Non c'è ancora",
            "cc_night_from" => "Dalle {from}",
            "cc_listening" => "In ascolto",
            "cc_linked" => "Collegato",
            k => k,
        }
        .to_string()
    }

    #[test]
    fn six_tiles_in_order() {
        let s = ControlState {
            wifi: Some("Casa".into()),
            airplay: Some("iPhone".into()),
            mic_on: true,
            night_from: "23:00".into(),
            ha: Some(true),
            ..Default::default()
        };
        let t = tiles(&s, &w);
        let keys: Vec<&str> = t.iter().map(|t| t.key).collect();
        assert_eq!(keys, ["wifi", "bluetooth", "airplay", "mic", "night", "ha"]);
        assert_eq!((t[0].status.as_str(), t[0].on), ("Casa", true));
        assert!(!t[0].tappable && t[2].tappable);
        assert!(!t[1].enabled, "no Bluetooth yet");
        assert_eq!((t[2].status.as_str(), t[2].on), ("iPhone", true));
        assert_eq!(t[4].status, "Dalle 23:00");
        assert_eq!(t[5].status, "Collegato");
    }

    #[test]
    fn airplay_ready_and_off() {
        let mut s = ControlState {
            airplay: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(tiles(&s, &w)[2].status, "Pronto");
        s.airplay = None;
        assert!(!tiles(&s, &w)[2].on);
    }
}
