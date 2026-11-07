// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The speaker volume in percent, as the volume keys, the screen and (later) Home Assistant
//! see it, put on the card's amplifier control (`[audio] volume_control`, on the ThinkSmart View
//! the TAS5782M's 'Speaker Amp Master': 0-85, 1 dB per step, 85 = 0 dB).
//!
//! The percentage moves in `volume_step` steps between `volume_step` % and 100 %. The curve is
//! linear in the control's steps, so linear in dB on a dB control: the lowest step is
//! `volume_min_amp`, 100 % is `volume_max_amp` (the ceiling: the body never writes more). With
//! the defaults (5 %, 47, 85) every step is exactly 2 dB, 5 % = -38 dB and 100 % = 0 dB (the
//! level Lenovo's own firmware always played at; the ceiling was chosen by ear, 2026-11-04).
//!
//! Without the control (the simulator, another card, `volume_control = ""`) the same curve
//! goes on the software gain of the mix: 100 % = `[audio] volume`, every step below it the same
//! dB lower. The audio crate never touches mixer controls (its own rule): this module does.

use std::time::{Duration, Instant};

use audio::Player;

/// What asks for a change: a volume key (one step) or a value (Home Assistant over MQTT, M2b;
/// nothing sends it yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VolumeRequest {
    /// +1 up, -1 down; `repeat` = generated while the key is held.
    Step { dir: i32, repeat: bool },
    /// A percentage (snapped to the steps).
    Set(u8),
    /// A percentage as it is, off the steps (the AirPlay sender's slider: its 16 steps of
    /// 6.25 % would land 5 and 10 % apart on ours). Within the lowest step..100.
    Exact(u8),
}

/// Percent ↔ control value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Curve {
    /// The control's value at the lowest step.
    pub min_amp: i64,
    /// The control's value at 100 % (the ceiling).
    pub max_amp: i64,
    /// Step in percent (also the lowest percentage).
    pub step: u8,
}

impl Curve {
    fn lowest(&self) -> u8 {
        self.step.clamp(1, 100)
    }

    /// `p` snapped to the steps, within lowest..=100.
    pub fn snap(&self, p: u8) -> u8 {
        let s = self.lowest() as u32;
        let p = p.min(100) as u32;
        (((p + s / 2) / s) * s).clamp(s, 100) as u8
    }

    /// The control's value for `p` percent.
    pub fn amp(&self, p: u8) -> i64 {
        let lo = self.lowest() as f64;
        let p = (p.min(100) as f64).max(lo);
        if lo >= 100.0 {
            return self.max_amp;
        }
        let k = (p - lo) / (100.0 - lo);
        let v = self.min_amp as f64 + (self.max_amp - self.min_amp) as f64 * k;
        (v.round() as i64).min(self.max_amp)
    }

    /// The percentage (on the steps) closest to the control's value `amp`.
    pub fn percent(&self, amp: i64) -> u8 {
        let lo = self.lowest() as f64;
        if self.max_amp <= self.min_amp {
            return 100;
        }
        let k = (amp - self.min_amp) as f64 / (self.max_amp - self.min_amp) as f64;
        let p = lo + k * (100.0 - lo);
        self.snap(p.clamp(0.0, 100.0).round() as u8)
    }

    /// One step up (`dir` > 0) or down from `p` (a value off the steps goes to the next one).
    pub fn step_from(&self, p: u8, dir: i32) -> u8 {
        let s = self.lowest() as i32;
        let p = p.min(100) as i32;
        let n = if dir > 0 {
            (p / s + 1) * s
        } else if dir < 0 {
            ((p + s - 1) / s - 1) * s
        } else {
            p
        };
        n.clamp(s, 100) as u8
    }
}

/// Where the level goes.
pub enum Output {
    /// The card's control (ALSA simple mixer element).
    #[cfg(target_os = "linux")]
    Amp(Amp),
    /// The software gain of the mix: 100 % = `base`.
    Gain { player: Player, base: f32 },
    /// Nowhere (tests): the value is only remembered.
    Memory(i64),
}

/// The amplifier control through the ALSA mixer API.
#[cfg(target_os = "linux")]
pub struct Amp {
    mixer: alsa::mixer::Mixer,
    id: alsa::mixer::SelemId,
    name: String,
    card: String,
    range: (i64, i64),
}

#[cfg(target_os = "linux")]
impl Amp {
    pub fn open(card: &str, control: &str) -> Result<Amp, String> {
        use alsa::mixer::{Mixer, SelemId};
        let mixer = Mixer::new(card, false).map_err(|e| format!("mixer {card}: {e}"))?;
        let id = SelemId::new(control, 0);
        let range = {
            let s = mixer
                .find_selem(&id)
                .ok_or_else(|| format!("mixer {card}: no control '{control}'"))?;
            if !s.has_playback_volume() {
                return Err(format!("mixer {card}: '{control}' has no playback volume"));
            }
            s.get_playback_volume_range()
        };
        Ok(Amp {
            mixer,
            id,
            name: control.into(),
            card: card.into(),
            range,
        })
    }

    pub fn range(&self) -> (i64, i64) {
        self.range
    }

    /// The control's value now (re-read: someone may have used amixer).
    pub fn get(&self) -> Result<i64, String> {
        let _ = self.mixer.handle_events();
        let s = self
            .mixer
            .find_selem(&self.id)
            .ok_or_else(|| format!("'{}' is gone", self.name))?;
        s.get_playback_volume(alsa::mixer::SelemChannelId::mono())
            .map_err(|e| format!("'{}': {e}", self.name))
    }

    /// The control's value in dB, if it has a dB scale.
    pub fn db(&self, v: i64) -> Option<f32> {
        let s = self.mixer.find_selem(&self.id)?;
        s.ask_playback_vol_db(v).ok().map(|m| m.to_db())
    }

    pub fn set(&self, v: i64) -> Result<(), String> {
        let s = self
            .mixer
            .find_selem(&self.id)
            .ok_or_else(|| format!("'{}' is gone", self.name))?;
        s.set_playback_volume_all(v)
            .map_err(|e| format!("'{}' = {v}: {e}", self.name))
    }
}

/// The volume settings (`[audio]`).
#[derive(Debug, Clone)]
pub struct VolumeConfig {
    /// "" = the software gain.
    pub control: String,
    pub card: String,
    pub curve: Curve,
    /// `[audio] volume`: the software gain at 100 % when there is no control.
    pub base_gain: f32,
}

/// The current volume and where it goes (owned by the core thread).
pub struct Volume {
    curve: Curve,
    percent: u8,
    out: Output,
    /// Changed and not saved yet (saved a second after the last change).
    dirty_since: Option<Instant>,
}

impl Volume {
    /// Opens the output and applies the saved level; without one, the level found on the
    /// control (above the ceiling it is lowered to the ceiling).
    pub fn open(cfg: &VolumeConfig, player: &Player, saved: Option<u8>) -> Volume {
        let gain = || Output::Gain {
            player: player.clone(),
            base: cfg.base_gain,
        };
        #[cfg(target_os = "linux")]
        let out = if cfg.control.is_empty() {
            gain()
        } else {
            match Amp::open(&cfg.card, &cfg.control) {
                Ok(a) => Output::Amp(a),
                Err(e) => {
                    log::warn!("volume: {e}; the software gain is used");
                    gain()
                }
            }
        };
        #[cfg(not(target_os = "linux"))]
        let out = gain();
        Volume::with_output(cfg.curve, out, saved)
    }

    pub fn with_output(curve: Curve, out: Output, saved: Option<u8>) -> Volume {
        let mut curve = curve;
        #[cfg(target_os = "linux")]
        if let Output::Amp(a) = &out {
            let (lo, hi) = a.range();
            curve.max_amp = curve.max_amp.clamp(lo, hi);
            curve.min_amp = curve.min_amp.clamp(lo, curve.max_amp);
        }
        let mut v = Volume {
            curve,
            percent: 100,
            out,
            dirty_since: None,
        };
        let found = v.read();
        match (saved, found) {
            (Some(p), _) => {
                v.percent = curve.snap(p);
                v.write();
                v.log("restored");
            }
            (None, Some(a)) => {
                v.percent = curve.percent(a);
                if a > curve.max_amp {
                    v.write();
                    v.log("lowered to the ceiling");
                } else {
                    v.log("found");
                }
            }
            (None, None) => {
                v.write();
                v.log("set");
            }
        }
        v
    }

    fn read(&self) -> Option<i64> {
        match &self.out {
            #[cfg(target_os = "linux")]
            Output::Amp(a) => a.get().map_err(|e| log::warn!("volume: {e}")).ok(),
            Output::Gain { .. } => None,
            Output::Memory(v) => Some(*v),
        }
    }

    fn write(&mut self) {
        let amp = self.curve.amp(self.percent);
        match &mut self.out {
            #[cfg(target_os = "linux")]
            Output::Amp(a) => {
                if let Err(e) = a.set(amp) {
                    log::warn!("volume: {e}");
                }
            }
            Output::Gain { player, base } => {
                let db = (amp - self.curve.max_amp) as f32;
                player.set_volume(*base * 10f32.powf(db / 20.0));
            }
            Output::Memory(v) => *v = amp,
        }
    }

    fn log(&self, what: &str) {
        let amp = self.curve.amp(self.percent);
        let (to, db) = match &self.out {
            #[cfg(target_os = "linux")]
            Output::Amp(a) => (format!("{} '{}'", a.card, a.name), a.db(amp)),
            Output::Gain { .. } => ("software gain".to_string(), None),
            Output::Memory(_) => ("memory".to_string(), None),
        };
        let db = db.map_or(String::new(), |d| format!(", {d:.0} dB"));
        log::info!(
            event = "volume",
            percent = self.percent as u64,
            amp = amp;
            "volume {}% ({what}): {to} = {amp}{db}",
            self.percent
        );
    }

    pub fn percent(&self) -> u8 {
        self.percent
    }

    pub fn curve(&self) -> Curve {
        self.curve
    }

    /// The control's value written for the current percentage.
    pub fn amp(&self) -> i64 {
        self.curve.amp(self.percent)
    }

    /// Applies a request; returns whether the level changed.
    pub fn apply(&mut self, req: VolumeRequest, now: Instant) -> bool {
        let new = match req {
            VolumeRequest::Step { dir, .. } => self.curve.step_from(self.percent, dir),
            VolumeRequest::Set(p) => self.curve.snap(p),
            VolumeRequest::Exact(p) => p.clamp(self.curve.step.clamp(1, 100), 100),
        };
        if new == self.percent {
            return false;
        }
        self.percent = new;
        self.write();
        self.dirty_since = Some(now);
        log::debug!("volume {}% = {}", new, self.amp());
        true
    }

    /// The percentage to save, once `quiet` passed since the last change (or at once with
    /// `quiet` = 0, at shutdown).
    pub fn take_unsaved(&mut self, now: Instant, quiet: Duration) -> Option<u8> {
        let t = self.dirty_since?;
        if now.duration_since(t) < quiet {
            return None;
        }
        self.dirty_since = None;
        self.log("saved");
        Some(self.percent)
    }

    #[cfg(test)]
    fn memory(&self) -> Option<i64> {
        match self.out {
            Output::Memory(v) => Some(v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: Curve = Curve {
        min_amp: 47,
        max_amp: 85,
        step: 5,
    };

    #[test]
    fn two_db_per_step_up_to_the_ceiling() {
        assert_eq!(C.amp(5), 47);
        assert_eq!(C.amp(40), 61);
        assert_eq!(C.amp(100), 85);
        for p in (5..100).step_by(5) {
            assert_eq!(C.amp(p + 5) - C.amp(p), 2, "{p}");
        }
        // Never above the ceiling, never below the floor.
        assert_eq!(C.amp(0), 47);
        assert_eq!(C.amp(255), 85);
    }

    #[test]
    fn percent_is_the_inverse_on_the_steps() {
        for p in (5..=100).step_by(5) {
            assert_eq!(C.percent(C.amp(p)), p);
        }
        assert_eq!(C.percent(0), 5);
        assert_eq!(C.percent(61), 40);
        assert_eq!(C.percent(85), 100);
        assert_eq!(C.percent(120), 100);
    }

    #[test]
    fn steps_snap_and_clamp() {
        assert_eq!(C.step_from(40, 1), 45);
        assert_eq!(C.step_from(40, -1), 35);
        assert_eq!(C.step_from(42, 1), 45);
        assert_eq!(C.step_from(42, -1), 40);
        assert_eq!(C.step_from(100, 1), 100);
        assert_eq!(C.step_from(5, -1), 5);
        assert_eq!(C.snap(0), 5);
        assert_eq!(C.snap(42), 40);
        assert_eq!(C.snap(43), 45);
        assert_eq!(C.snap(200), 100);
    }

    #[test]
    fn a_coarser_curve_stays_monotonic() {
        let c = Curve {
            min_amp: 47,
            max_amp: 61,
            step: 5,
        };
        let mut last = 0;
        for p in (5..=100).step_by(5) {
            assert!(c.amp(p) >= last);
            last = c.amp(p);
        }
        assert_eq!(last, 61);
    }

    #[test]
    fn found_level_and_ceiling() {
        // No saved level: the control's value is kept (as a percentage).
        let v = Volume::with_output(C, Output::Memory(61), None);
        assert_eq!(v.percent(), 40);
        assert_eq!(v.memory(), Some(61));
        // Above the ceiling (a control with a wider range): lowered to 100 % = 85.
        let v = Volume::with_output(C, Output::Memory(100), None);
        assert_eq!(v.percent(), 100);
        assert_eq!(v.memory(), Some(85));
    }

    #[test]
    fn save_and_restore() {
        let t0 = Instant::now();
        let mut v = Volume::with_output(C, Output::Memory(85), Some(30));
        assert_eq!((v.percent(), v.memory()), (30, Some(57)));
        assert!(v.apply(
            VolumeRequest::Step {
                dir: 1,
                repeat: false
            },
            t0
        ));
        assert!(v.apply(
            VolumeRequest::Step {
                dir: 1,
                repeat: true
            },
            t0
        ));
        assert_eq!((v.percent(), v.memory()), (40, Some(61)));
        let quiet = Duration::from_secs(1);
        assert_eq!(v.take_unsaved(t0 + Duration::from_millis(500), quiet), None);
        assert_eq!(v.take_unsaved(t0 + quiet, quiet), Some(40));
        assert_eq!(v.take_unsaved(t0 + quiet * 2, quiet), None);
        // At the top nothing changes and nothing is to be saved.
        let mut v = Volume::with_output(C, Output::Memory(85), Some(100));
        assert!(!v.apply(
            VolumeRequest::Step {
                dir: 1,
                repeat: false
            },
            t0
        ));
        assert_eq!(v.take_unsaved(t0, Duration::ZERO), None);
        assert!(v.apply(VolumeRequest::Set(42), t0));
        assert_eq!((v.percent(), v.memory()), (40, Some(61)));
    }

    #[test]
    fn exact_is_not_snapped() {
        let mut v = Volume::with_output(C, Output::Memory(0), Some(40));
        assert!(v.apply(VolumeRequest::Exact(58), Instant::now()));
        assert_eq!(v.percent(), 58);
        v.apply(VolumeRequest::Exact(0), Instant::now());
        assert_eq!(v.percent(), C.step);
        v.apply(VolumeRequest::Exact(58), Instant::now());
        v.apply(
            VolumeRequest::Step {
                dir: 1,
                repeat: false,
            },
            Instant::now(),
        );
        assert_eq!(v.percent(), 60);
    }
}
