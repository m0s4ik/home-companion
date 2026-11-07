// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The slot vocabularies of each language: every spoken form of a duration, a time of day and a
//! percentage, with its value. A grammar template like "metti un timer di {duration}" expands
//! to one phrase per entry. Kept finite on purpose: Vosk's closed grammar is a list of phrases.

/// The values a slot can take, each with all its spoken forms.
#[derive(Debug, Clone, Default)]
pub struct Slots {
    /// "dodici minuti e mezzo" → 750 s.
    pub durations: Vec<(String, u64)>,
    /// With its preposition: "alle sette e mezza" → (7, 30), "all'una di notte" → (1, 0).
    pub times: Vec<(String, (u8, u8))>,
    /// "cinquanta per cento", "cinquanta" → 50.
    pub percents: Vec<(String, u8)>,
}

/// The slots of a language ("it", "en"); None for a language without reflexes.
pub fn for_language(code: &str) -> Option<Slots> {
    match code {
        "it" => Some(italian()),
        "en" => Some(english()),
        _ => None,
    }
}

// ---- Italian ------------------------------------------------------------------------------

const IT_UNITS: [&str; 20] = [
    "zero",
    "uno",
    "due",
    "tre",
    "quattro",
    "cinque",
    "sei",
    "sette",
    "otto",
    "nove",
    "dieci",
    "undici",
    "dodici",
    "tredici",
    "quattordici",
    "quindici",
    "sedici",
    "diciassette",
    "diciotto",
    "diciannove",
];

const IT_TENS: [&str; 10] = [
    "",
    "",
    "venti",
    "trenta",
    "quaranta",
    "cinquanta",
    "sessanta",
    "settanta",
    "ottanta",
    "novanta",
];

/// 0..=100 in Italian words: "ventuno", "ventotto", "trentatré", "cento".
pub fn it_number(n: u32) -> String {
    match n {
        0..=19 => IT_UNITS[n as usize].to_string(),
        100 => "cento".to_string(),
        20..=99 => {
            let tens = IT_TENS[(n / 10) as usize];
            let u = n % 10;
            if u == 0 {
                return tens.to_string();
            }
            // The tens lose their last vowel before "uno" and "otto"; "tre" takes the accent.
            let tens = if u == 1 || u == 8 {
                &tens[..tens.len() - 1]
            } else {
                tens
            };
            let unit = if u == 3 { "tré" } else { IT_UNITS[u as usize] };
            format!("{tens}{unit}")
        }
        _ => n.to_string(),
    }
}

fn italian() -> Slots {
    let w = it_number;
    let mut d: Vec<(String, u64)> = Vec::new();
    // Seconds.
    for n in 2..=59 {
        d.push((format!("{} secondi", w(n)), n as u64));
    }
    // Minutes, half minutes, a few "N minuti e S secondi".
    d.push(("un minuto".into(), 60));
    d.push(("un minuto e mezzo".into(), 90));
    for n in 2..=59 {
        d.push((format!("{} minuti", w(n)), n as u64 * 60));
        d.push((format!("{} minuti e mezzo", w(n)), n as u64 * 60 + 30));
    }
    for s in [10u32, 15, 20, 30, 40, 45, 50] {
        d.push((format!("un minuto e {} secondi", w(s)), 60 + s as u64));
        for n in 2..=9 {
            d.push((
                format!("{} minuti e {} secondi", w(n), w(s)),
                n as u64 * 60 + s as u64,
            ));
        }
    }
    // Hours and their parts.
    d.push(("mezz'ora".into(), 1800));
    d.push(("un quarto d'ora".into(), 900));
    d.push(("tre quarti d'ora".into(), 2700));
    d.push(("un'ora".into(), 3600));
    d.push(("un'ora e mezza".into(), 5400));
    d.push(("un'ora e un quarto".into(), 4500));
    d.push(("un'ora e un minuto".into(), 3660));
    for m in 2..=59 {
        d.push((format!("un'ora e {} minuti", w(m)), 3600 + m as u64 * 60));
    }
    for h in 2..=12 {
        d.push((format!("{} ore", w(h)), h as u64 * 3600));
        d.push((format!("{} ore e mezza", w(h)), h as u64 * 3600 + 1800));
    }
    for h in 2..=3 {
        for m in [5u32, 10, 15, 20, 25, 30, 40, 45, 50] {
            d.push((
                format!("{} ore e {} minuti", w(h), w(m)),
                h as u64 * 3600 + m as u64 * 60,
            ));
        }
    }

    // Times of day, with the preposition ("alle", "all'", "a").
    let hour = |h: u32| -> String {
        match h {
            1 => "all'una".into(),
            _ => format!("alle {}", w(h)),
        }
    };
    let mut t: Vec<(String, (u8, u8))> = Vec::new();
    let mut push_time = |s: String, h: u32, m: u32| t.push((s, ((h % 24) as u8, m as u8)));
    for h in 1..=23u32 {
        let base = hour(h);
        push_time(base.clone(), h, 0);
        push_time(format!("{base} in punto"), h, 0);
        push_time(format!("{base} e mezza"), h, 30);
        push_time(format!("{base} e mezzo"), h, 30);
        push_time(format!("{base} e un quarto"), h, 15);
        push_time(format!("{base} meno un quarto"), (h + 23) % 24, 45);
        for m in 1..=59 {
            push_time(format!("{base} e {}", w(m)), h, m);
        }
        for m in [5u32, 10, 20] {
            push_time(format!("{base} meno {}", w(m)), (h + 23) % 24, 60 - m);
        }
    }
    // Midnight and noon.
    for (word, h) in [("a mezzanotte", 0u32), ("a mezzogiorno", 12)] {
        push_time(word.to_string(), h, 0);
        push_time(format!("{word} e mezza"), h, 30);
        push_time(format!("{word} e un quarto"), h, 15);
        push_time(format!("{word} meno un quarto"), (h + 23) % 24, 45);
    }
    // The 12-hour forms with a part of the day: "alle sette di sera" = 19:00.
    for h in 1..=12u32 {
        let base = hour(h);
        for (suffix, m, back) in [
            ("", 0u32, false),
            (" e mezza", 30, false),
            (" e un quarto", 15, false),
            (" meno un quarto", 45, true),
        ] {
            let at = if back { (h + 11) % 12 } else { h % 12 };
            // "Alle dodici di mattina" is not said (noon is "mezzogiorno"); "meno un quarto" is fine.
            let mut parts = Vec::new();
            if h != 12 || back {
                parts.push((" di mattina", at));
            }
            if (1..=11).contains(&h) {
                parts.push((" di sera", at + 12));
            }
            if (1..=7).contains(&h) {
                parts.push((" del pomeriggio", at + 12));
            }
            if (9..=12).contains(&h) {
                parts.push((" di notte", if h == 12 && !back { 0 } else { at + 12 }));
            } else if (1..=5).contains(&h) {
                parts.push((" di notte", at));
            }
            for (part, hh) in parts {
                push_time(format!("{base}{suffix}{part}"), hh, m);
            }
        }
    }

    let mut p: Vec<(String, u8)> = Vec::new();
    for n in 0..=100u32 {
        p.push((format!("{} per cento", w(n)), n as u8));
        p.push((w(n), n as u8));
    }
    Slots {
        durations: d,
        times: t,
        percents: p,
    }
}

// ---- English ------------------------------------------------------------------------------

const EN_UNITS: [&str; 20] = [
    "zero",
    "one",
    "two",
    "three",
    "four",
    "five",
    "six",
    "seven",
    "eight",
    "nine",
    "ten",
    "eleven",
    "twelve",
    "thirteen",
    "fourteen",
    "fifteen",
    "sixteen",
    "seventeen",
    "eighteen",
    "nineteen",
];

const EN_TENS: [&str; 10] = [
    "", "", "twenty", "thirty", "forty", "fifty", "sixty", "seventy", "eighty", "ninety",
];

/// 0..=100 in English words as Vosk writes them ("twenty one", "one hundred").
pub fn en_number(n: u32) -> String {
    match n {
        0..=19 => EN_UNITS[n as usize].to_string(),
        100 => "one hundred".to_string(),
        20..=99 => {
            let tens = EN_TENS[(n / 10) as usize];
            match n % 10 {
                0 => tens.to_string(),
                u => format!("{tens} {}", EN_UNITS[u as usize]),
            }
        }
        _ => n.to_string(),
    }
}

fn english() -> Slots {
    let w = en_number;
    let mut d: Vec<(String, u64)> = Vec::new();
    for n in 2..=59 {
        d.push((format!("{} seconds", w(n)), n as u64));
    }
    d.push(("one minute".into(), 60));
    d.push(("a minute".into(), 60));
    d.push(("a minute and a half".into(), 90));
    d.push(("one and a half minutes".into(), 90));
    for n in 2..=59 {
        d.push((format!("{} minutes", w(n)), n as u64 * 60));
        d.push((format!("{} and a half minutes", w(n)), n as u64 * 60 + 30));
    }
    d.push(("half an hour".into(), 1800));
    d.push(("a quarter of an hour".into(), 900));
    d.push(("an hour".into(), 3600));
    d.push(("one hour".into(), 3600));
    d.push(("an hour and a half".into(), 5400));
    for h in 2..=12 {
        d.push((format!("{} hours", w(h)), h as u64 * 3600));
    }

    let mut t: Vec<(String, (u8, u8))> = Vec::new();
    for h in 1..=12u32 {
        let base = format!("at {}", w(h));
        // (phrase, minute, back): `back` = a quarter before the named hour.
        let mut forms: Vec<(String, u32, bool)> = vec![
            (base.clone(), 0, false),
            (format!("{base} o'clock"), 0, false),
            (format!("at half past {}", w(h)), 30, false),
            (format!("at quarter past {}", w(h)), 15, false),
            (format!("at a quarter past {}", w(h)), 15, false),
            (format!("at quarter to {}", w(h)), 0, true),
            (format!("at a quarter to {}", w(h)), 0, true),
        ];
        for m in 1..=59 {
            let mw = if m < 10 { format!("oh {}", w(m)) } else { w(m) };
            forms.push((format!("{base} {mw}"), m, false));
        }
        // The named hour on the 24-hour clock for each part of the day: bare and "in the morning"
        // keep it, "in the afternoon/evening" add 12. Twelve is noon, bare or "in the afternoon";
        // "twelve in the morning/evening" is ambiguous and not generated.
        let parts: [(&str, Option<u32>); 4] = if h == 12 {
            [
                ("", Some(12)),
                (" in the morning", None),
                (" in the afternoon", Some(12)),
                (" in the evening", None),
            ]
        } else {
            [
                ("", Some(h)),
                (" in the morning", Some(h)),
                (" in the afternoon", Some(h + 12)),
                (" in the evening", Some(h + 12)),
            ]
        };
        for (s, m, back) in forms {
            for (part, at) in parts {
                let Some(at) = at else { continue };
                let total = (at * 60 + m + 24 * 60 - if back { 15 } else { 0 }) % (24 * 60);
                t.push((
                    format!("{s}{part}"),
                    ((total / 60) as u8, (total % 60) as u8),
                ));
            }
        }
    }
    t.push(("at midnight".into(), (0, 0)));
    t.push(("at noon".into(), (12, 0)));

    let mut p: Vec<(String, u8)> = Vec::new();
    for n in 0..=100u32 {
        p.push((format!("{} percent", w(n)), n as u8));
        p.push((w(n), n as u8));
    }
    Slots {
        durations: d,
        times: t,
        percents: p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value<T: Copy>(list: &[(String, T)], s: &str) -> Option<T> {
        list.iter().find(|(k, _)| k == s).map(|(_, v)| *v)
    }

    #[test]
    fn italian_numbers() {
        assert_eq!(it_number(1), "uno");
        assert_eq!(it_number(21), "ventuno");
        assert_eq!(it_number(23), "ventitré");
        assert_eq!(it_number(28), "ventotto");
        assert_eq!(it_number(40), "quaranta");
        assert_eq!(it_number(59), "cinquantanove");
        assert_eq!(it_number(100), "cento");
    }

    #[test]
    fn english_numbers() {
        assert_eq!(en_number(21), "twenty one");
        assert_eq!(en_number(100), "one hundred");
    }

    #[test]
    fn italian_durations() {
        let s = italian();
        let d = |x| value(&s.durations, x);
        assert_eq!(d("dieci minuti"), Some(600));
        assert_eq!(d("un minuto e mezzo"), Some(90));
        assert_eq!(d("dodici minuti e mezzo"), Some(750));
        assert_eq!(d("un'ora"), Some(3600));
        assert_eq!(d("trenta secondi"), Some(30));
        assert_eq!(d("mezz'ora"), Some(1800));
        assert_eq!(d("un quarto d'ora"), Some(900));
        assert_eq!(d("tre minuti e trenta secondi"), Some(210));
        assert_eq!(d("un'ora e venti minuti"), Some(4800));
        assert_eq!(d("due ore e mezza"), Some(9000));
        assert_eq!(d("uno minuti"), None);
    }

    #[test]
    fn italian_times() {
        let s = italian();
        let t = |x| value(&s.times, x);
        assert_eq!(t("alle sette"), Some((7, 0)));
        assert_eq!(t("alle sette e mezza"), Some((7, 30)));
        assert_eq!(t("alle sei e quaranta"), Some((6, 40)));
        assert_eq!(t("all'una"), Some((1, 0)));
        assert_eq!(t("alle otto meno un quarto"), Some((7, 45)));
        assert_eq!(t("alle sette meno dieci"), Some((6, 50)));
        assert_eq!(t("alle sette di sera"), Some((19, 0)));
        assert_eq!(t("alle tre del pomeriggio"), Some((15, 0)));
        assert_eq!(t("alle undici di notte"), Some((23, 0)));
        assert_eq!(t("alle tre di notte"), Some((3, 0)));
        assert_eq!(t("a mezzanotte"), Some((0, 0)));
        assert_eq!(t("alle ventitré e trenta"), Some((23, 30)));
        assert_eq!(t("alle sette e un quarto di mattina"), Some((7, 15)));
    }

    #[test]
    fn english_slots() {
        let s = english();
        assert_eq!(value(&s.durations, "twelve and a half minutes"), Some(750));
        assert_eq!(value(&s.times, "at seven thirty"), Some((7, 30)));
        assert_eq!(value(&s.times, "at seven oh five"), Some((7, 5)));
        assert_eq!(value(&s.times, "at quarter to eight"), Some((7, 45)));
        assert_eq!(value(&s.times, "at seven in the evening"), Some((19, 0)));
        assert_eq!(value(&s.percents, "fifty percent"), Some(50));
    }

    #[test]
    fn no_spoken_form_twice() {
        for code in ["it", "en"] {
            let s = for_language(code).unwrap();
            for list in [
                s.durations.iter().map(|(k, _)| k).collect::<Vec<_>>(),
                s.times.iter().map(|(k, _)| k).collect(),
                s.percents.iter().map(|(k, _)| k).collect(),
            ] {
                let mut seen = std::collections::HashSet::new();
                for k in list {
                    assert!(seen.insert(k), "{code}: {k:?} twice");
                }
            }
        }
    }

    // Added with agy (Gemini), checked by hand.
    #[test]
    fn italian_numbers_units_and_teens() {
        assert_eq!(it_number(0), "zero");
        assert_eq!(it_number(2), "due");
        assert_eq!(it_number(3), "tre");
        assert_eq!(it_number(10), "dieci");
        assert_eq!(it_number(14), "quattordici");
        assert_eq!(it_number(17), "diciassette");
        assert_eq!(it_number(19), "diciannove");
    }

    #[test]
    fn italian_numbers_tens_and_compounds() {
        assert_eq!(it_number(20), "venti");
        assert_eq!(it_number(30), "trenta");
        assert_eq!(it_number(31), "trentuno");
        assert_eq!(it_number(33), "trentatré");
        assert_eq!(it_number(38), "trentotto");
        assert_eq!(it_number(48), "quarantotto");
        assert_eq!(it_number(50), "cinquanta");
        assert_eq!(it_number(53), "cinquantatré");
    }

    #[test]
    fn english_numbers_0_to_59() {
        assert_eq!(en_number(0), "zero");
        assert_eq!(en_number(1), "one");
        assert_eq!(en_number(11), "eleven");
        assert_eq!(en_number(12), "twelve");
        assert_eq!(en_number(15), "fifteen");
        assert_eq!(en_number(20), "twenty");
        assert_eq!(en_number(30), "thirty");
        assert_eq!(en_number(45), "forty five");
        assert_eq!(en_number(59), "fifty nine");
    }

    #[test]
    fn italian_durations_minutes_and_seconds() {
        let s = italian();
        let d = |x| value(&s.durations, x);
        assert_eq!(d("un minuto"), Some(60));
        assert_eq!(d("due minuti"), Some(120));
        assert_eq!(d("due secondi"), Some(2));
        assert_eq!(d("cinquantanove secondi"), Some(59));
        assert_eq!(d("un minuto e dieci secondi"), Some(70));
        assert_eq!(d("un minuto e quarantacinque secondi"), Some(105));
        assert_eq!(d("nove minuti e cinquanta secondi"), Some(590));
    }

    #[test]
    fn italian_durations_hours_and_fractions() {
        let s = italian();
        let d = |x| value(&s.durations, x);
        assert_eq!(d("tre quarti d'ora"), Some(2700));
        assert_eq!(d("un'ora e mezza"), Some(5400));
        assert_eq!(d("un'ora e un quarto"), Some(4500));
        assert_eq!(d("un'ora e un minuto"), Some(3660));
        assert_eq!(d("due ore"), Some(7200));
        assert_eq!(d("dodici ore"), Some(43200));
        assert_eq!(d("dodici ore e mezza"), Some(45000));
    }

    #[test]
    fn italian_durations_hours_and_minutes() {
        let s = italian();
        let d = |x| value(&s.durations, x);
        assert_eq!(d("due ore e cinque minuti"), Some(7500));
        assert_eq!(d("due ore e dieci minuti"), Some(7800));
        assert_eq!(d("tre ore e quindici minuti"), Some(11700));
        assert_eq!(d("tre ore e cinquanta minuti"), Some(13800));
        assert_eq!(d("un'ora e due minuti"), Some(3720));
        assert_eq!(d("un'ora e cinquantanove minuti"), Some(7140));
    }

    #[test]
    fn italian_durations_rejected() {
        let s = italian();
        let d = |x| value(&s.durations, x);
        assert_eq!(d("un ora"), None);
        assert_eq!(d("un ora e mezza"), None);
        assert_eq!(d("un secondo"), None);
        assert_eq!(d("sessanta secondi"), None);
        assert_eq!(d("sessanta minuti"), None);
        assert_eq!(d("quattro ore e dieci minuti"), None);
        assert_eq!(d("due ore e sette minuti"), None);
        assert_eq!(d("due ore e un minuto"), None);
        assert_eq!(d("tredici ore"), None);
        assert_eq!(d("dieci minuti e trenta secondi"), None);
        assert_eq!(d("un minuto e cinque secondi"), None);
        assert_eq!(d("due ore e un quarto"), None);
    }

    #[test]
    fn italian_clock_times_basic_and_quarter() {
        let s = italian();
        let t = |x| value(&s.times, x);
        assert_eq!(t("alle sette e un quarto"), Some((7, 15)));
        assert_eq!(t("all'una in punto"), Some((1, 0)));
        assert_eq!(t("all'una e mezza"), Some((1, 30)));
        assert_eq!(t("all'una e un quarto"), Some((1, 15)));
        assert_eq!(t("alle due in punto"), Some((2, 0)));
        assert_eq!(t("alle tre e un quarto"), Some((3, 15)));
        assert_eq!(t("alle quattro e mezzo"), Some((4, 30)));
    }

    #[test]
    fn italian_clock_times_noon_and_midnight() {
        let s = italian();
        let t = |x| value(&s.times, x);
        assert_eq!(t("a mezzogiorno"), Some((12, 0)));
        assert_eq!(t("a mezzogiorno e mezza"), Some((12, 30)));
        assert_eq!(t("a mezzogiorno e un quarto"), Some((12, 15)));
        assert_eq!(t("a mezzogiorno meno un quarto"), Some((11, 45)));
        assert_eq!(t("a mezzanotte e mezza"), Some((0, 30)));
        assert_eq!(t("a mezzanotte e un quarto"), Some((0, 15)));
        assert_eq!(t("a mezzanotte meno un quarto"), Some((23, 45)));
    }

    #[test]
    fn italian_clock_times_subtraction_and_24h() {
        let s = italian();
        let t = |x| value(&s.times, x);
        assert_eq!(t("all'una meno un quarto"), Some((0, 45)));
        assert_eq!(t("all'una meno dieci"), Some((0, 50)));
        assert_eq!(t("all'una meno venti"), Some((0, 40)));
        assert_eq!(t("all'una meno cinque"), Some((0, 55)));
        assert_eq!(t("alle due meno cinque"), Some((1, 55)));
        assert_eq!(t("alle due meno venti"), Some((1, 40)));
        assert_eq!(t("alle venti"), Some((20, 0)));
        assert_eq!(t("alle ventidue in punto"), Some((22, 0)));
        assert_eq!(t("alle ventitré e cinquantanove"), Some((23, 59)));
    }

    #[test]
    fn italian_clock_times_parts_of_day() {
        let s = italian();
        let t = |x| value(&s.times, x);
        assert_eq!(t("all'una di notte"), Some((1, 0)));
        assert_eq!(t("alle quattro di mattina"), Some((4, 0)));
        assert_eq!(t("alle quattro del pomeriggio"), Some((16, 0)));
        assert_eq!(t("alle cinque e mezza del pomeriggio"), Some((17, 30)));
        assert_eq!(t("alle otto di sera"), Some((20, 0)));
        assert_eq!(t("alle nove e un quarto di sera"), Some((21, 15)));
        assert_eq!(t("alle dieci meno un quarto di sera"), Some((21, 45)));
        assert_eq!(t("alle dieci di notte"), Some((22, 0)));
        assert_eq!(t("alle dodici di notte"), Some((0, 0)));
    }

    #[test]
    fn italian_clock_times_rejected() {
        let s = italian();
        let t = |x| value(&s.times, x);
        assert_eq!(t("sette"), None);
        assert_eq!(t("sette e mezza"), None);
        assert_eq!(t("mezzogiorno"), None);
        assert_eq!(t("mezzanotte"), None);
        assert_eq!(t("alle zero"), None);
        assert_eq!(t("alle ventiquattro"), None);
        assert_eq!(t("alle venticinque"), None);
        assert_eq!(t("alle sette meno quindici"), None);
        assert_eq!(t("alle sette meno trenta"), None);
        assert_eq!(t("alle quattordici di sera"), None);
    }

    #[test]
    fn italian_percents() {
        let s = italian();
        let p = |x| value(&s.percents, x);
        assert_eq!(p("zero per cento"), Some(0));
        assert_eq!(p("zero"), Some(0));
        assert_eq!(p("uno per cento"), Some(1));
        assert_eq!(p("cinque per cento"), Some(5));
        assert_eq!(p("cento per cento"), Some(100));
        assert_eq!(p("cento"), Some(100));
        assert_eq!(p("ventuno per cento"), Some(21));
        assert_eq!(p("trentatré per cento"), Some(33));
        assert_eq!(p("quaranta"), Some(40));
        assert_eq!(p("centouno per cento"), None);
        assert_eq!(p("centouno"), None);
    }

    #[test]
    fn english_durations_seconds_and_minutes() {
        let s = english();
        let d = |x| value(&s.durations, x);
        assert_eq!(d("two seconds"), Some(2));
        assert_eq!(d("thirty seconds"), Some(30));
        assert_eq!(d("fifty nine seconds"), Some(59));
        assert_eq!(d("one minute"), Some(60));
        assert_eq!(d("a minute"), Some(60));
        assert_eq!(d("a minute and a half"), Some(90));
        assert_eq!(d("one and a half minutes"), Some(90));
        assert_eq!(d("two minutes"), Some(120));
        assert_eq!(d("two and a half minutes"), Some(150));
        assert_eq!(d("fifty nine minutes"), Some(3540));
    }

    #[test]
    fn english_durations_hours() {
        let s = english();
        let d = |x| value(&s.durations, x);
        assert_eq!(d("half an hour"), Some(1800));
        assert_eq!(d("a quarter of an hour"), Some(900));
        assert_eq!(d("an hour"), Some(3600));
        assert_eq!(d("one hour"), Some(3600));
        assert_eq!(d("an hour and a half"), Some(5400));
        assert_eq!(d("two hours"), Some(7200));
        assert_eq!(d("twelve hours"), Some(43200));
    }

    #[test]
    fn english_durations_rejected() {
        let s = english();
        let d = |x| value(&s.durations, x);
        assert_eq!(d("one second"), None);
        assert_eq!(d("a second"), None);
        assert_eq!(d("sixty seconds"), None);
        assert_eq!(d("sixty minutes"), None);
        assert_eq!(d("thirteen hours"), None);
        assert_eq!(d("three quarters of an hour"), None);
        assert_eq!(d("two and a half hours"), None);
        assert_eq!(d("two hours and a half"), None);
        assert_eq!(d("one hour and twenty minutes"), None);
        assert_eq!(d("one minute and thirty seconds"), None);
    }

    #[test]
    fn english_clock_times_basic() {
        let s = english();
        let t = |x| value(&s.times, x);
        assert_eq!(t("at one"), Some((1, 0)));
        assert_eq!(t("at one o'clock"), Some((1, 0)));
        assert_eq!(t("at half past two"), Some((2, 30)));
        assert_eq!(t("at quarter past three"), Some((3, 15)));
        assert_eq!(t("at a quarter past four"), Some((4, 15)));
        assert_eq!(t("at quarter to five"), Some((4, 45)));
        assert_eq!(t("at a quarter to six"), Some((5, 45)));
        assert_eq!(t("at quarter to one"), Some((0, 45)));
        assert_eq!(t("at noon"), Some((12, 0)));
        assert_eq!(t("at midnight"), Some((0, 0)));
    }

    #[test]
    fn english_clock_times_minutes_and_parts_of_day() {
        let s = english();
        let t = |x| value(&s.times, x);
        assert_eq!(t("at eight oh one"), Some((8, 1)));
        assert_eq!(t("at eight fifteen"), Some((8, 15)));
        assert_eq!(t("at eight forty five"), Some((8, 45)));
        assert_eq!(t("at eight fifty nine"), Some((8, 59)));
        assert_eq!(t("at eight in the morning"), Some((8, 0)));
        assert_eq!(t("at two in the afternoon"), Some((14, 0)));
        assert_eq!(t("at six in the evening"), Some((18, 0)));
        assert_eq!(t("at half past two in the afternoon"), Some((14, 30)));
        assert_eq!(t("at quarter past eight in the evening"), Some((20, 15)));
    }

    #[test]
    fn english_clock_times_rejected() {
        let s = english();
        let t = |x| value(&s.times, x);
        assert_eq!(t("seven"), None);
        assert_eq!(t("seven o'clock"), None);
        assert_eq!(t("at seven five"), None);
        assert_eq!(t("at thirteen"), None);
        assert_eq!(t("at thirteen o'clock"), None);
        assert_eq!(t("noon"), None);
        assert_eq!(t("midnight"), None);
        assert_eq!(t("at seven at night"), None);
        assert_eq!(t("at half to eight"), None);
        assert_eq!(t("at twenty to eight"), None);
    }

    #[test]
    fn for_language_and_english_percents() {
        assert!(for_language("it").is_some());
        assert!(for_language("en").is_some());
        assert!(for_language("fr").is_none());
        assert!(for_language("de").is_none());
        assert!(for_language("").is_none());

        let s = english();
        let p = |x| value(&s.percents, x);
        assert_eq!(p("zero percent"), Some(0));
        assert_eq!(p("zero"), Some(0));
        assert_eq!(p("one percent"), Some(1));
        assert_eq!(p("one hundred percent"), Some(100));
        assert_eq!(p("one hundred"), Some(100));
        assert_eq!(p("twenty five percent"), Some(25));
        assert_eq!(p("ninety nine"), Some(99));
        assert_eq!(p("one hundred and one percent"), None);
        assert_eq!(p("one hundred and one"), None);
    }

    #[test]
    fn twelve_is_noon_unless_said_otherwise() {
        let en = english();
        let t = |x| value(&en.times, x);
        assert_eq!(t("at twelve"), Some((12, 0)));
        assert_eq!(t("at twelve o'clock"), Some((12, 0)));
        assert_eq!(t("at half past twelve"), Some((12, 30)));
        assert_eq!(t("at quarter to twelve"), Some((11, 45)));
        assert_eq!(t("at quarter to twelve in the afternoon"), Some((11, 45)));
        assert_eq!(t("at twelve in the morning"), None);
        assert_eq!(t("at quarter to one"), Some((0, 45)));
        assert_eq!(t("at quarter to one in the afternoon"), Some((12, 45)));
        assert_eq!(t("at quarter to seven in the evening"), Some((18, 45)));
        let it = italian();
        let t = |x| value(&it.times, x);
        assert_eq!(t("alle dodici di mattina"), None);
        assert_eq!(t("alle dodici meno un quarto di mattina"), Some((11, 45)));
        assert_eq!(t("alle dodici di notte"), Some((0, 0)));
    }
}
