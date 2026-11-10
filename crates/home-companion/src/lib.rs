// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! home-companion's body, `home-companion`: a Wyoming satellite for Home Assistant with the wake
//! word on the device, the companion screen and local timers. The binary (src/main.rs) wires
//! the threads; the pieces live here so the tests can drive them.

pub mod airplay;
pub mod alarms;
pub mod args;
pub mod brain_link;
pub mod config;
pub mod conversation;
pub mod core;
pub mod cues;
pub mod ha;
pub mod history;
pub mod logger;
pub mod reflex;
pub mod repeat;
pub mod settings;
pub mod spectrum;
pub mod subcommands;
pub mod suspend;
pub mod timers;
pub mod ui;
pub mod usage;
pub mod voice;
pub mod volume;
pub mod wake_thread;

pub mod signals {
    //! SIGINT, SIGTERM and SIGHUP only set a flag; every loop checks it and leaves normally,
    //! so the console mode, the audio and the Wyoming server are restored and closed. SIGUSR1
    //! wakes the screen as a touch would (a test hook: `systemctl kill -s USR1 home-companion`); SIGUSR2
    //! saves the screen as it is now as PNG in `[ui] screenshot_dir`
    //! (`systemctl kill -s USR2 home-companion`: Home Assistant's view included).

    use std::sync::atomic::{AtomicBool, Ordering};

    pub static STOP: AtomicBool = AtomicBool::new(false);
    pub static WAKE: AtomicBool = AtomicBool::new(false);
    pub static SHOT: AtomicBool = AtomicBool::new(false);
    /// Set with STOP: after the normal shutdown the program executes itself again (the
    /// first start picked Home Assistant: what needs its address is wired only at start).
    pub static RESTART: AtomicBool = AtomicBool::new(false);

    /// A clean stop, then the same program again with the same arguments (main.rs).
    pub fn request_restart() {
        RESTART.store(true, Ordering::Relaxed);
        STOP.store(true, Ordering::Relaxed);
    }

    pub fn restart_requested() -> bool {
        RESTART.load(Ordering::Relaxed)
    }

    extern "C" fn on_signal(_: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }

    extern "C" fn on_wake(_: libc::c_int) {
        WAKE.store(true, Ordering::Relaxed);
    }

    extern "C" fn on_shot(_: libc::c_int) {
        SHOT.store(true, Ordering::Relaxed);
    }

    pub fn install() {
        // SAFETY: the handler only stores to an atomic.
        unsafe {
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_signal as *const () as usize;
            libc::sigemptyset(&mut sa.sa_mask);
            for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                libc::sigaction(sig, &sa, std::ptr::null_mut());
            }
            sa.sa_sigaction = on_wake as *const () as usize;
            libc::sigaction(libc::SIGUSR1, &sa, std::ptr::null_mut());
            sa.sa_sigaction = on_shot as *const () as usize;
            libc::sigaction(libc::SIGUSR2, &sa, std::ptr::null_mut());
        }
    }

    pub fn stop_requested() -> bool {
        STOP.load(Ordering::Relaxed)
    }

    /// True once per SIGUSR2.
    pub fn take_shot() -> bool {
        SHOT.swap(false, Ordering::Relaxed)
    }

    /// True once per SIGUSR1.
    pub fn take_wake() -> bool {
        WAKE.swap(false, Ordering::Relaxed)
    }
}

/// Exit codes (architecture section 5).
pub mod exit {
    pub const OK: u8 = 0;
    pub const USAGE: u8 = 1;
    pub const SUBSYSTEM: u8 = 2;
    pub const RENDERER: u8 = 3;
    pub const DATA: u8 = 4;
    /// Not an error: the first start saved Home Assistant's address and the body must start
    /// again (under systemd, `Restart=on-failure` does it and pulls in the units it wants, the
    /// web helper among them; elsewhere the program executes itself).
    pub const RESTART: u8 = 5;
}

/// Loads the wake model named by the configuration and builds the `Info` Home Assistant gets.
pub fn wake_model(cfg: &config::Config) -> Result<wakeword::Model, String> {
    if cfg.wake_word.model.is_empty() {
        return Err(format!(
            "no wake word model: {}/wakeword has no *.json and wake_word.model is empty",
            cfg.character.dir
        ));
    }
    wakeword::Model::load(std::path::Path::new(&cfg.wake_word.model)).map_err(|e| e.to_string())
}

pub fn satellite_info(cfg: &config::Config, model: &wakeword::Model) -> wyoming::Info {
    let m = &model.manifest;
    let attribution = wyoming::Attribution {
        name: m.author.clone().unwrap_or_else(|| "unknown".into()),
        url: m.website.clone().unwrap_or_default(),
    };
    wyoming::Info::for_satellite(wyoming::SatelliteInfo {
        name: cfg.satellite.name.clone(),
        area: Some(cfg.satellite.area.clone()).filter(|a| !a.is_empty()),
        description: Some("home-companion".into()),
        version: env!("CARGO_PKG_VERSION").into(),
        attribution: wyoming::Attribution {
            name: "home-companion".into(),
            url: "https://github.com/m0s4ik/home-companion".into(),
        },
        has_vad: false,
        wake_program_name: "home-companion-microwakeword".into(),
        wake_models: vec![wyoming::WakeModelInfo {
            name: model.name.clone(),
            phrase: model.phrase().to_string(),
            languages: model.languages(),
            attribution,
            description: Some(model.phrase().to_string()),
            version: Some(m.version.to_string()),
        }],
    })
}
