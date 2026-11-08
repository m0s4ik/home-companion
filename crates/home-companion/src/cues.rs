// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body's sounds. A character brings its own as `<character>/sounds/<name>.wav` (Alfred's
//! are made by `tools/make-sounds.py`: soft struck notes; replace any file by hand); a missing or
//! unreadable file falls back to a plain synthesized tone, so the body always has every sound.

use std::path::Path;
use std::sync::Arc;

use audio::Clip;

/// Peak level of the fallback tones: 0.5 of full scale = -6 dBFS (before the volume).
pub const AMP: f32 = 0.5;

/// Every sound the body makes, by what it means.
pub struct Cues {
    /// Wake word heard (or Talk): "I'm listening".
    pub wake: Arc<Clip>,
    /// Something went wrong, or Home Assistant does not answer.
    pub error: Arc<Clip>,
    /// Not a reflex and nobody else to ask (no Home Assistant).
    pub not_understood: Arc<Clip>,
    /// One ring of the timer bell (repeated with a gap).
    pub bell: Arc<Clip>,
    /// A volume key: one short tick at the new level.
    pub volume: Arc<Clip>,
    /// Reflexes, each with its own sound so it is clear the body did it.
    pub timer_set: Arc<Clip>,
    pub alarm_set: Arc<Clip>,
    pub cancelled: Arc<Clip>,
    pub time: Arc<Clip>,
    pub done: Arc<Clip>,
    /// Something done that the user waited for (linked to Home Assistant).
    pub success: Arc<Clip>,
    pub silence: Arc<Clip>,
    /// A reminder or a message card.
    pub reminder: Arc<Clip>,
    /// A control of the screen pressed: a very soft tick.
    pub tap: Arc<Clip>,
    /// Back home from Home Assistant's view.
    pub home: Arc<Clip>,
    /// Each of a timer's last ten seconds: a very soft clock tick.
    pub tick: Arc<Clip>,
}

impl Cues {
    /// The plain synthesized tones only.
    pub fn new() -> Cues {
        Cues {
            wake: Clip::tones(&[(660.0, 120), (880.0, 160)], 0, AMP),
            error: Clip::tones(&[(880.0, 120), (660.0, 160)], 0, AMP),
            not_understood: Clip::tones(&[(330.0, 90), (330.0, 120)], 60, AMP),
            bell: Clip::tones(&[(1319.0, 150), (1319.0, 150), (1319.0, 400)], 80, AMP),
            volume: Clip::tones(&[(880.0, 90)], 0, AMP),
            timer_set: Clip::tones(&[(1319.0, 60), (1319.0, 60), (1760.0, 200)], 50, AMP),
            alarm_set: Clip::tones(&[(523.0, 200), (784.0, 300)], 30, AMP),
            cancelled: Clip::tones(&[(784.0, 100), (659.0, 100), (523.0, 180)], 20, AMP),
            time: Clip::tones(&[(784.0, 200), (1047.0, 300)], 60, AMP),
            done: Clip::tones(&[(523.0, 90), (659.0, 90), (784.0, 160)], 10, AMP),
            success: Clip::tones(
                &[(523.0, 70), (659.0, 70), (784.0, 70), (1047.0, 300)],
                10,
                AMP,
            ),
            silence: Clip::tones(&[(262.0, 150)], 0, AMP),
            reminder: Clip::tones(&[(659.0, 120), (784.0, 120), (1047.0, 200)], 30, AMP),
            tap: Clip::tones(&[(1200.0, 15)], 0, AMP * 0.3),
            home: Clip::tones(&[(784.0, 90), (523.0, 160)], 10, AMP * 0.6),
            tick: Clip::tones(&[(1500.0, 12)], 0, AMP * 0.25),
        }
    }

    /// The character's `sounds/` directory over the plain tones.
    pub fn load(dir: &Path) -> Cues {
        let mut c = Cues::new();
        let mut found = 0;
        for (name, slot) in [
            ("wake", &mut c.wake),
            ("error", &mut c.error),
            ("not-understood", &mut c.not_understood),
            ("bell", &mut c.bell),
            ("volume", &mut c.volume),
            ("timer-set", &mut c.timer_set),
            ("alarm-set", &mut c.alarm_set),
            ("cancelled", &mut c.cancelled),
            ("time", &mut c.time),
            ("done", &mut c.done),
            ("success", &mut c.success),
            ("silence", &mut c.silence),
            ("reminder", &mut c.reminder),
            ("tap", &mut c.tap),
            ("home", &mut c.home),
            ("tick", &mut c.tick),
        ] {
            let path = dir.join(format!("{name}.wav"));
            if !path.exists() {
                continue;
            }
            match Clip::from_wav(&path) {
                Ok(clip) => {
                    *slot = clip;
                    found += 1;
                }
                Err(e) => log::warn!("sound {}: {e}: the plain tone instead", path.display()),
            }
        }
        if found > 0 {
            log::info!("sounds: {found} from {}", dir.display());
        }
        c
    }
}

impl Default for Cues {
    fn default() -> Self {
        Cues::new()
    }
}
