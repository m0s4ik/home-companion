// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Configuration errors and the crate-wide rules.

mod common;

use audio::{Audio, AudioConfig, Backend, Error};
use std::path::Path;

fn try_start(cfg: &AudioConfig) -> Result<Audio, Error> {
    Audio::start(cfg, Box::new(|_| {}))
}

#[cfg(not(any(feature = "webrtc", feature = "webrtc-system")))]
#[test]
fn agc_without_webrtc_is_a_feature_error() {
    let cfg = AudioConfig {
        backend: Backend::Null,
        agc: true,
        ..AudioConfig::default()
    };
    assert!(matches!(try_start(&cfg), Err(Error::Feature("webrtc"))));
}

#[cfg(any(feature = "webrtc", feature = "webrtc-system"))]
#[test]
fn agc_raises_a_quiet_voice() {
    use std::time::Duration;
    // A quiet synthetic vowel (harmonics of ~130 Hz shaped by three formants), 0.6 s on and
    // 0.4 s off every second, at about -45 dBFS RMS while voiced. Band-limited noise is not
    // enough: AGC2 only adapts on what its voice detector takes for speech.
    let rate = 16000.0f64;
    let mut seed = 1u32;
    let quiet: Vec<i16> = (0..16000 * 8)
        .map(|i| {
            let t = i as f64 / rate;
            let ph = t % 1.0;
            let mut s = 0.0;
            if ph < 0.6 {
                let f0 = 130.0 + 20.0 * (2.0 * std::f64::consts::PI * 0.7 * t).sin();
                for k in 1..25 {
                    let f = f0 * k as f64;
                    let a = (-((f - 700.0) / 150.0).powi(2)).exp()
                        + 0.6 * (-((f - 1200.0) / 200.0).powi(2)).exp()
                        + 0.3 * (-((f - 2500.0) / 300.0).powi(2)).exp()
                        + 0.03 / k as f64;
                    s += a * (2.0 * std::f64::consts::PI * f * t).sin();
                }
                let env = 1f64.min(ph / 0.03).min((0.6 - ph) / 0.03);
                s *= env * 120.0;
            }
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            s += ((seed >> 16) % 7) as f64 - 3.0;
            s.round() as i16
        })
        .collect();
    let dir = common::tmpdir("agc");
    let p = dir.join("quiet.wav");
    common::write_wav(&p, 16000, 1, &quiet);
    let mut cfg = common::file_cfg(Some(p), None, false);
    cfg.capture_channels = 1;
    cfg.agc = true;
    let (audio, _ev) = common::start(&cfg);
    let rx = audio.subscribe(500);
    let frames: Vec<_> = (0..400)
        .map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap())
        .collect();
    audio.stop();
    // The last 4 s, voiced parts and pauses alike.
    let out: Vec<i16> = frames[200..400]
        .iter()
        .flat_map(|f| f.samples.iter().copied())
        .collect();
    let input = common::rms_dbfs(&quiet[200 * 320..400 * 320]);
    let output = common::rms_dbfs(&out);
    assert!(
        output > input + 6.0,
        "AGC gain: {input:.1} -> {output:.1} dBFS"
    );
}

#[test]
fn bad_capture_channels() {
    let cfg = AudioConfig {
        backend: Backend::Null,
        capture_channels: 3,
        ..AudioConfig::default()
    };
    assert!(matches!(try_start(&cfg), Err(Error::Unsupported(_))));
}

#[test]
fn missing_or_wrong_capture_file() {
    let cfg = common::file_cfg(Some("/nonexistent/x.wav".into()), None, false);
    assert!(matches!(try_start(&cfg), Err(Error::Io(_))));
    let cfg = common::file_cfg(Some(common::fixture("answer-22050.wav")), None, false);
    assert!(
        matches!(try_start(&cfg), Err(Error::Unsupported(_))),
        "22050 Hz capture"
    );
}

#[cfg(not(feature = "cpal"))]
#[test]
fn cpal_without_the_feature() {
    let cfg = AudioConfig {
        backend: Backend::Cpal,
        ..AudioConfig::default()
    };
    assert!(matches!(try_start(&cfg), Err(Error::Feature("cpal"))));
}

/// The spike's rule: the crate never touches the card's mixer controls.
#[test]
fn no_mixer_control_code() {
    fn walk(dir: &Path, hits: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, hits);
            } else if p.extension().is_some_and(|x| x == "rs") {
                let text = std::fs::read_to_string(&p).unwrap();
                for (i, line) in text.lines().enumerate() {
                    for pat in ["alsa::mixer", "Mixer::new", "Selem", "snd_mixer", "amixer"] {
                        if line.contains(pat) {
                            hits.push(format!("{}:{}: {line}", p.display(), i + 1));
                        }
                    }
                }
            }
        }
    }
    let mut hits = Vec::new();
    walk(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut hits,
    );
    assert!(
        hits.is_empty(),
        "mixer-control code found:\n{}",
        hits.join("\n")
    );
}

/// Every source file carries the SPDX header.
#[test]
fn spdx_headers() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut missing = Vec::new();
    for sub in [
        "src",
        "src/backend",
        "src/bin",
        "tests",
        "tests/common",
        "tools",
    ] {
        for e in std::fs::read_dir(root.join(sub)).unwrap() {
            let p = e.unwrap().path();
            if p.is_file() {
                let text = std::fs::read_to_string(&p).unwrap_or_default();
                if !text.contains("SPDX-License-Identifier: GPL-3.0-or-later")
                    || !text.contains("SPDX-FileCopyrightText: 2026 Andrea Mattera")
                {
                    missing.push(p.display().to_string());
                }
            }
        }
    }
    assert!(missing.is_empty(), "{missing:?}");
}
