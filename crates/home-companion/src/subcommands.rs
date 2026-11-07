// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The runbook's helpers: `audio-probe`, `mic-levels`, `wake-test`, `check-config`. None of
//! them plays a sound.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::config::{Config, Source};

/// What the pcm devices accept (hw_params only: nothing changes, nothing sounds).
pub fn audio_probe(cfg: &Config) -> Result<(), String> {
    for (dev, playback) in [
        (&cfg.audio.playback_device, true),
        (&cfg.audio.capture_device, false),
    ] {
        let dir = if playback { "playback" } else { "capture" };
        match audio::probe::alsa_formats(dev, playback) {
            Ok(json) => println!("{dir} {dev}:\n{json}"),
            Err(e) => println!("{dir} {dev}: {e}"),
        }
    }
    Ok(())
}

/// Levels of the left, right and average channel every second (to choose
/// `audio.capture_channel`): one JSON line per second, then a summary.
pub fn mic_levels(cfg: &Config, seconds: f64) -> Result<(), String> {
    let acfg = cfg.audio_config();
    let mut rows = Vec::new();
    audio::meter::capture_levels(
        &acfg,
        Duration::from_secs_f64(seconds),
        Duration::from_secs(1),
        |l| {
            println!(
                "{}",
                serde_json::json!({
                    "at_s": l.at_s,
                    "left_rms_dbfs": l.left_rms_dbfs, "left_peak": l.left_peak,
                    "right_rms_dbfs": l.right_rms_dbfs, "right_peak": l.right_peak,
                    "average_rms_dbfs": l.average_rms_dbfs, "average_peak": l.average_peak,
                })
            );
            rows.push((l.left_rms_dbfs, l.right_rms_dbfs, l.average_rms_dbfs));
        },
    )
    .map_err(|e| format!("mic-levels: {e}"))?;
    if !rows.is_empty() {
        let n = rows.len() as f32;
        let mean = |f: fn(&(f32, f32, f32)) -> f32| rows.iter().map(f).sum::<f32>() / n;
        println!(
            "mean rms dBFS over {} s: left {:.1}, right {:.1}, average {:.1} (now: capture_channel = {:?})",
            rows.len(),
            mean(|r| r.0),
            mean(|r| r.1),
            mean(|r| r.2),
            cfg.audio.capture_channel
        );
    }
    Ok(())
}

/// Runs the wake word detector over WAV files (16 kHz; channel = `audio.capture_channel`
/// for stereo files) and prints every detection, then the totals.
pub fn wake_test(cfg: &Config, files: &[PathBuf]) -> Result<u32, String> {
    let model = crate::wake_model(cfg)?;
    let channel = match cfg.audio.capture_channel.as_str() {
        "right" => Some(1),
        _ => Some(0),
    };
    let mut total = 0;
    for f in files {
        let samples =
            wakeword::wav::read_16k(f, channel).map_err(|e| format!("{}: {e}", f.display()))?;
        let mut d =
            wakeword::Detector::new(&model, &cfg.detector_config()).map_err(|e| e.to_string())?;
        let mut n = 0;
        let mut max_p = 0f32;
        for chunk in samples.chunks(audio::FRAME_SAMPLES) {
            if let Some(det) = d.feed(chunk).map_err(|e| e.to_string())? {
                println!(
                    "{}: {} at {:.2} s (p {:.3})",
                    f.display(),
                    det.name,
                    det.at.as_secs_f64(),
                    det.probability
                );
                n += 1;
            }
            max_p = max_p.max(d.last_probability());
        }
        let s = d.stats();
        let audio_s = s.samples as f64 / wakeword::SAMPLE_RATE as f64;
        println!(
            "{}: {n} detection(s) in {audio_s:.1} s, max probability {max_p:.3}, cpu {:.4} s per audio second, arena {} of {} bytes",
            f.display(),
            if audio_s > 0.0 {
                s.cpu_s / audio_s
            } else {
                0.0
            },
            d.arena_used(),
            d.arena_size()
        );
        total += n;
    }
    println!("total detections: {total}");
    Ok(total)
}

/// Loads everything a run needs and says what it found.
pub fn check_config(cfg: &Config, source: &Source) -> Result<(), String> {
    match source {
        Source::File(p) => println!("config: {}", p.display()),
        Source::Missing(p) => println!("config: {} not found, defaults used", p.display()),
    }
    let c = character::load(Path::new(&cfg.character.dir))
        .map_err(|e| format!("character {}: {}", cfg.character.dir, e.join("; ")))?;
    println!(
        "character: {} ({}), languages {}",
        c.name,
        cfg.character.dir,
        c.languages().collect::<Vec<_>>().join(", ")
    );
    let l = locale::load_or_english(Path::new(&cfg.paths.locales_dir), &cfg.character.language)
        .map_err(|e| format!("locale: {}", e.join("; ")))?;
    println!("locale: {} ({})", l.code, cfg.paths.locales_dir);
    let m = crate::wake_model(cfg)?;
    let d = wakeword::Detector::new(&m, &cfg.detector_config()).map_err(|e| e.to_string())?;
    println!(
        "wake word: {} \"{}\" ({}), arena {} of {} bytes",
        m.name,
        m.phrase(),
        cfg.wake_word.model,
        d.arena_used(),
        d.arena_size()
    );
    let nets = cfg.allowed_nets()?;
    println!(
        "listen: {}, allowed: {}",
        cfg.satellite.bind,
        if nets.is_empty() {
            "NOBODY (set satellite.allowed)".into()
        } else {
            cfg.satellite.allowed.join(", ")
        }
    );
    println!(
        "audio: {} (playback {}, capture {} x{} {})",
        cfg.audio.backend,
        cfg.audio.playback_device,
        cfg.audio.capture_device,
        cfg.audio.capture_channels,
        cfg.audio.capture_channel
    );
    println!(
        "ui: renderer {}, rotation {}, touch {:?}",
        cfg.ui.renderer, cfg.ui.rotation, cfg.ui.touch
    );
    println!("state: {}", cfg.paths.state_dir);
    println!("ok");
    Ok(())
}

/// `home-companion ha-setup check|repair [ROW]|unlink`: Settings → Home Assistant from the shell (each
/// row printed in the configured language as it changes).
pub fn ha_setup(cfg: &Config, action: &str, row: Option<&str>) -> Result<(), String> {
    use crate::ha::setup::{self, Level, Row, Scope};
    if cfg.home_assistant.url.is_empty() {
        return Err("no [home_assistant] url".into());
    }
    let locale =
        locale::load_or_english(Path::new(&cfg.paths.locales_dir), &cfg.character.language)
            .map_err(|e| e.join("; "))?;
    let name = cfg.satellite.name.clone();
    let show = |rows: &[Row]| {
        for r in rows {
            let mark = match r.level {
                Level::Ok => "ok  ",
                Level::Warn => "WARN",
                Level::Bad => "BAD ",
                Level::Skipped => "--  ",
                Level::Unknown => "?   ",
            };
            let label = locale
                .text(&format!("ha_row_{}", r.id))
                .replace("{name}", &name);
            let detail = r.detail(|k| locale.text(k).replace("{name}", &name));
            let repair = if r.repair { "  [repair]" } else { "" };
            println!("{mark} {label}: {detail}{repair}");
        }
        println!();
    };
    let auth = std::sync::Arc::new(cfg.ha_auth());
    let sc = cfg.setup_config(auth.clone())?;
    let url = &cfg.home_assistant.url;
    let ep = crate::ha::endpoint(url)?;
    let host = crate::ha::link::local_address(&ep.host, ep.port).map_err(|e| e.to_string())?;
    let body = setup::Body {
        name: sc.name.clone(),
        host: host.to_string(),
        wyoming_port: sc.wyoming_port,
        mcp_port: sc.mcp_port,
        prompt: sc.prompt.clone(),
    };
    let token = auth.access_token().map_err(|e| e.to_string())?;
    let mut api = setup::HaApi::connect(url, &token)?;
    let mut st = setup::State::load(&sc.state_file);
    let brain = crate::brain_link::brain_info;
    match action {
        "check" => show(&setup::check(&mut api, &st, &body, brain)),
        "repair" => {
            let scope = row.map_or(Scope::Missing, |r| Scope::Row(r.to_string()));
            let mut last = String::new();
            let mut progress = |rows: &[Row]| {
                let now = format!("{rows:?}");
                if now != last {
                    show(rows);
                    last = now;
                }
            };
            setup::repair(
                &mut api,
                &mut st,
                &sc.state_file,
                &body,
                brain,
                scope,
                &mut progress,
            )?;
        }
        "unlink" => {
            let errors = setup::unlink(&mut api, &st);
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
            let _ = std::fs::remove_file(&sc.state_file);
            drop(api);
            auth.unlink();
            println!("unlinked");
        }
        _ => unreachable!(),
    }
    Ok(())
}
