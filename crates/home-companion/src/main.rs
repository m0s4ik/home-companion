// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! `home-companion`: home-companion's body. See `home-companion --help` and packaging/README.md.
//!
//! Start-up order matters: the screen (renderer) is set up first, on the main thread, before
//! any other thread exists and before the sound card and the port are opened, so a failed GPU
//! start can re-execute the program with the software renderer.

use std::path::Path;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use home_companion::args::{self, Command};
use home_companion::config::{self, Config, Source};
use home_companion::core::{Core, CoreConfig, CoreEvent, Deps, MicControl, UiEvent};
use home_companion::settings::Settings;
use home_companion::timers::{Timers, now_unix};
use home_companion::ui::Screen;
use home_companion::ui::backlight::Levels;
use home_companion::ui::renderer::{self, Kind, Prepared, UiContext};
use home_companion::ui::screen::{Demo, ScreenOptions, UiOptions};
use home_companion::{exit, logger, signals, subcommands, wake_thread};

fn main() -> ExitCode {
    let a = match args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) => {
            if !e.is_empty() {
                eprintln!("home-companion: {e}");
            }
            eprintln!("{}", args::USAGE);
            return ExitCode::from(if e.is_empty() { exit::OK } else { exit::USAGE });
        }
    };
    match a.command {
        Command::Version => {
            println!(
                "home-companion {} (wyoming {})",
                env!("CARGO_PKG_VERSION"),
                wyoming::PROTOCOL_VERSION
            );
            return ExitCode::SUCCESS;
        }
        Command::ResetConsole => {
            return match home_companion::ui::platform_fb::reset_console() {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("home-companion: {e}");
                    ExitCode::from(exit::SUBSYSTEM)
                }
            };
        }
        _ => {}
    }
    let (mut cfg, source) = match config::load(a.config.as_deref(), a.simulator) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("home-companion: config: {e}");
            return ExitCode::from(exit::USAGE);
        }
    };
    if let Some(r) = &a.renderer {
        cfg.ui.renderer = r.clone();
    }
    let logger = match logger::Logger::new(&cfg.log.format, &cfg.log.level, &cfg.log.file) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("home-companion: {e}");
            return ExitCode::from(exit::USAGE);
        }
    };
    let _ = logger.install();
    match &source {
        Source::File(p) => log::info!("config {}", p.display()),
        Source::Missing(p) => log::warn!("{} not found: defaults used", p.display()),
    }
    // The language and Home Assistant's address chosen on the screen (settings.toml).
    let saved = Settings::in_dir(&cfg.state_dir());
    let first = home_companion::settings::apply(&mut cfg, &saved);
    log::debug!("effective configuration:\n{}", cfg.to_toml());
    let result = match a.command {
        Command::PrintConfig => {
            print!("{}", cfg.to_toml());
            Ok(())
        }
        Command::CheckConfig => subcommands::check_config(&cfg, &source),
        Command::WebLogin => home_companion::ui::webview::login_line(&cfg).map(|l| println!("{l}")),
        Command::WebUrl => home_companion::ui::webview::url_line(&cfg.home_assistant, &cfg.web)
            .map(|l| println!("{l}")),
        Command::HaSetup {
            ref action,
            ref row,
        } => subcommands::ha_setup(&cfg, action, row.as_deref()),
        Command::AudioProbe => subcommands::audio_probe(&cfg),
        Command::MicLevels { seconds } => subcommands::mic_levels(&cfg, seconds),
        Command::WakeTest { ref files } => subcommands::wake_test(&cfg, files).map(|_| ()),
        Command::Run => {
            let code = run(&a, cfg, first);
            if signals::restart_requested() {
                return restart();
            }
            return code;
        }
        Command::Version | Command::ResetConsole => unreachable!(),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("home-companion: {e}");
            ExitCode::from(exit::DATA)
        }
    }
}

/// Starts again after a clean shutdown (the first start saved Home Assistant's address: the
/// satellite, the link, the setup and the view are wired only at start). Under systemd the
/// exit code [`exit::RESTART`] lets `Restart=on-failure` start the unit again, which also pulls
/// in the units that want it (home-companion-web, skipped while there was no address); run by
/// hand, the program executes itself with the same arguments.
fn restart() -> ExitCode {
    use std::os::unix::process::CommandExt;
    if std::env::var_os("INVOCATION_ID").is_some() {
        log::info!(event = "restart"; "Home Assistant's address saved: exit {} for systemd to start the body again", exit::RESTART);
        log::logger().flush();
        return ExitCode::from(exit::RESTART);
    }
    log::info!(event = "restart"; "Home Assistant's address saved: starting again");
    log::logger().flush();
    let err = match std::env::current_exe() {
        Ok(exe) => std::process::Command::new(exe)
            .args(std::env::args_os().skip(1))
            .exec()
            .to_string(),
        Err(e) => format!("current_exe: {e}"),
    };
    log::error!("restart: {err}");
    ExitCode::from(exit::RESTART)
}

fn run(a: &args::Args, cfg: Config, first: home_companion::settings::FirstStart) -> ExitCode {
    // Before any thread exists (environment changes are not thread safe).
    if (a.simulator || cfg.ui.renderer == "window")
        && std::env::var_os("SLINT_SCALE_FACTOR").is_none()
    {
        // The glyphs are pre-rendered at the .slint sizes; scale 1 shows the device's pixels.
        // SAFETY: no other thread exists yet.
        unsafe { std::env::set_var("SLINT_SCALE_FACTOR", "1") };
    }
    signals::install();
    if std::env::var_os("HOME_COMPANION_RENDERER_FALLBACK").is_some() {
        log::warn!(event = "renderer_fallback"; "running with the software renderer after the GPU renderer failed");
    }

    // Data first: nothing runs without the character, the words and the wake model.
    let character = match character::load(Path::new(&cfg.character.dir)) {
        Ok(c) => Rc::new(c),
        Err(e) => {
            log::error!("character {}: {}", cfg.character.dir, e.join("; "));
            return ExitCode::from(exit::DATA);
        }
    };
    let state_dir = cfg.state_dir();
    let settings = Settings::in_dir(&state_dir);
    let language = cfg.character.language.clone();
    let model = match home_companion::wake_model(&cfg) {
        Ok(m) => m,
        Err(e) => {
            log::error!("wake word: {e}");
            return ExitCode::from(exit::DATA);
        }
    };
    let info = home_companion::satellite_info(&cfg, &model);
    let (bind, allowed) = match (cfg.bind_addr(), cfg.allowed_nets()) {
        (Ok(b), Ok(n)) => (b, n),
        (Err(e), _) | (_, Err(e)) => {
            log::error!("{e}");
            return ExitCode::from(exit::USAGE);
        }
    };
    if allowed.is_empty()
        && home_companion::ha::setup::State::load(
            &cfg.state_dir().join(home_companion::ha::setup::STATE_FILE),
        )
        .ha_addresses
        .is_empty()
    {
        log::warn!(
            "satellite.allowed is empty and Home Assistant is not linked yet: nobody can connect \
             until the QR link (or put Home Assistant's address there)"
        );
    }

    // The screen.
    let kind = renderer::choose(&cfg.ui.renderer);
    let rotation = renderer::rotation(&cfg.ui.rotation, kind);
    let demo = a.state.as_deref().and_then(Demo::parse);
    let opts = UiOptions {
        locales: cfg.paths.locales_dir.clone().into(),
        language,
        // The simulator shows the sample weather unless Home Assistant gives the real one.
        sample_weather: kind == Kind::Window
            && weather_config(a, &cfg, std::sync::Arc::new(cfg.ha_auth())).is_none(),
        demo,
        screen: ScreenOptions {
            backlight: cfg.screen.backlight.clone(),
            levels: Levels {
                idle: cfg.screen.idle_brightness,
                active: cfg.screen.active_brightness,
            },
            night_levels: Levels {
                idle: cfg.screen.night_brightness,
                active: cfg.screen.night_active_brightness,
            },
            auto: cfg.brightness_curve(),
            awake_seconds: cfg.screen.awake_seconds,
            animations: cfg.screen.animations,
            loading_seconds: cfg.screen.loading_seconds,
        },
        state_dir: a.state.is_none().then(|| state_dir.clone()),
        ha_linked: cfg.ha_linked(),
        ha_host: cfg.ha_host(),
        setup_at_start: matches!(kind, Kind::Gpu | Kind::Software),
        first_start: first,
        link: (a.state.is_none() && !cfg.home_assistant.url.is_empty()).then(|| {
            home_companion::ha::link::LinkSetup {
                hass_url: cfg.home_assistant.url.clone(),
                port: cfg.home_assistant.link_port,
                state_dir: state_dir.clone(),
                name: String::new(),
                done_text: String::new(),
                failed_text: String::new(),
            }
        }),
    };
    match kind {
        #[cfg(feature = "gpu")]
        Kind::Gpu => {
            if kind == Kind::Gpu
                && rotation != 0
                && std::env::var_os("SLINT_KMS_ROTATION").is_none()
            {
                // SAFETY: no other thread exists yet.
                unsafe { std::env::set_var("SLINT_KMS_ROTATION", rotation.to_string()) };
            }
            match renderer::prepare_gpu(rotation) {
                Ok(p) => go(
                    a, &cfg, p, character, opts, model, info, bind, allowed, settings,
                ),
                Err(why) => {
                    let e = renderer::reexec_software(&why);
                    log::error!("{e}");
                    ExitCode::from(exit::RENDERER)
                }
            }
        }
        #[cfg(not(feature = "gpu"))]
        Kind::Gpu => {
            let e = renderer::reexec_software("built without the gpu feature");
            log::error!("{e}");
            ExitCode::from(exit::RENDERER)
        }
        Kind::Window => match renderer::prepare_window() {
            Ok(p) => go(
                a, &cfg, p, character, opts, model, info, bind, allowed, settings,
            ),
            Err(e) => {
                log::error!("{e}");
                ExitCode::from(exit::RENDERER)
            }
        },
        Kind::Software | Kind::Headless => {
            match renderer::prepare_fb(kind, Path::new(&cfg.ui.framebuffer), rotation) {
                Ok(p) => go(
                    a, &cfg, p, character, opts, model, info, bind, allowed, settings,
                ),
                Err(e) => {
                    log::error!("{e}");
                    ExitCode::from(exit::RENDERER)
                }
            }
        }
    }
}

/// Starts audio, the wake word thread, the Wyoming server and the core, then runs the screen.
#[allow(clippy::too_many_arguments)]
fn go<S: Screen>(
    a: &args::Args,
    cfg: &Config,
    prepared: Prepared<S>,
    character: Rc<character::Character>,
    opts: UiOptions,
    model: wakeword::Model,
    info: wyoming::Info,
    bind: std::net::SocketAddr,
    allowed: Vec<wyoming::Net>,
    settings: Settings,
) -> ExitCode {
    let (core_tx, core_rx) = mpsc::channel::<CoreEvent>();
    let (ui_tx, ui_rx) = mpsc::channel();
    let mic = Arc::new(MicControl::default());

    // Touch (device only).
    let touch = if matches!(prepared.kind, Kind::Gpu | Kind::Software) && !cfg.ui.touch.is_empty() {
        let dev = if cfg.ui.touch == "auto" {
            home_companion::ui::touch::find_device()
        } else {
            Some(cfg.ui.touch.clone().into())
        };
        match dev {
            Some(d) => {
                let (tx, rx) = mpsc::channel();
                match home_companion::ui::touch::spawn(&d, prepared.rotation, tx, || {}) {
                    Ok(_) => Some(rx),
                    Err(e) => {
                        log::warn!("touch: {e}");
                        None
                    }
                }
            }
            None => {
                log::warn!("touch: no multi-touch device in /dev/input");
                None
            }
        }
    } else {
        None
    };

    // Volume keys (volume and wake) and the accelerometer (wake), device only.
    let wake_sources = if matches!(prepared.kind, Kind::Gpu | Kind::Software) {
        let (tx, rx) = mpsc::channel();
        if cfg.screen.keys {
            home_companion::ui::sensors::spawn_keys(
                &home_companion::ui::sensors::find_key_devices(),
                &tx,
                Some(core_tx.clone()),
            );
        }
        // The light sensor, read by the motion thread every 2 s (automatic brightness).
        let light = if cfg.screen.auto_brightness {
            match home_companion::ui::light::find_bus() {
                Some(bus) => match home_companion::ui::light::Als::open(&bus) {
                    Ok(a) => Some(a),
                    Err(e) => {
                        log::warn!("light sensor: {e}: fixed brightness");
                        None
                    }
                },
                None => {
                    log::info!("light sensor: none found (fixed brightness)");
                    None
                }
            }
        } else {
            None
        };
        if cfg.screen.motion_threshold > 0.0 {
            match home_companion::ui::sensors::find_accelerometer(&cfg.screen.accelerometer) {
                Some(dir) => home_companion::ui::sensors::spawn_motion(
                    dir,
                    cfg.screen.motion_threshold,
                    Duration::from_millis(500),
                    tx,
                    light,
                ),
                None if !cfg.screen.accelerometer.is_empty() => {
                    log::info!("accelerometer: none found")
                }
                None => {}
            }
        }
        Some(rx)
    } else {
        None
    };

    // Audio.
    let tx = core_tx.clone();
    let audio = match audio::Audio::start(
        &cfg.audio_config(),
        Box::new(move |e| {
            let _ = tx.send(CoreEvent::Audio(e));
        }),
    ) {
        Ok(a) => a,
        Err(e) => {
            log::error!(event = "audio_error", error = e.to_string().as_str(); "audio: {e}");
            return ExitCode::from(exit::SUBSYSTEM);
        }
    };
    log::info!(
        "audio: {} (output {} Hz)",
        cfg.audio.backend,
        audio.output_rate()
    );
    let player = audio.player();
    let volume =
        home_companion::volume::Volume::open(&cfg.volume_config(), &player, settings.volume());
    let recorder = audio.recorder();
    let frames = audio.subscribe(16);

    // Wake word.
    let detector = match wakeword::Detector::new(&model, &cfg.detector_config()) {
        Ok(d) => d,
        Err(e) => {
            log::error!("wake word {}: {e}", model.path.display());
            audio.stop();
            return ExitCode::from(exit::DATA);
        }
    };
    log::info!(
        event = "wake_model",
        name = model.name.as_str(),
        arena_used = detector.arena_used() as u64,
        arena = detector.arena_size() as u64;
        "wake word {} \"{}\": arena {} of {} bytes, one inference every {} slices",
        model.name,
        model.phrase(),
        detector.arena_used(),
        detector.arena_size(),
        detector.stride()
    );
    // The stop words ("basta", "stop"): optional; without them Stop is a touch only. Each model
    // keeps its own cutoff and window from its manifest.
    let mut stop_detectors = Vec::new();
    if cfg.wake_word.stop_model != "off" {
        for path in cfg
            .wake_word
            .stop_model
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            let loaded = wakeword::Model::load(std::path::Path::new(path))
                .map_err(|e| e.to_string())
                .and_then(|m| {
                    let mut dc = cfg.detector_config();
                    dc.cutoff = None;
                    wakeword::Detector::new(&m, &dc)
                        .map(|d| (m, d))
                        .map_err(|e| e.to_string())
                });
            match loaded {
                Ok((m, d)) => {
                    log::info!(event = "stop_model", name = m.name.as_str(), arena_used = d.arena_used() as u64; "stop word {} \"{}\": arena {} of {} bytes, heard while thinking, speaking or ringing", m.name, m.phrase(), d.arena_used(), d.arena_size());
                    stop_detectors.push(d);
                }
                Err(e) => log::warn!(event = "stop_model_error"; "stop word {path}: {e}: skipped"),
            }
        }
    }
    let wake_stats = Arc::new(Mutex::new(None));
    let wake = match wake_thread::spawn(
        frames,
        Some(detector),
        stop_detectors,
        mic.clone(),
        core_tx.clone(),
        wake_stats.clone(),
    ) {
        Ok(h) => h,
        Err(e) => {
            log::error!("wake word thread: {e}");
            audio.stop();
            return ExitCode::from(exit::SUBSYSTEM);
        }
    };

    // Wyoming. Home Assistant's own addresses, learned by the setup after the QR link, may
    // connect too (no `satellite.allowed` to write by hand).
    let learned: home_companion::ha::setup::Learned = Default::default();
    home_companion::ha::setup::set_learned(
        &Some(learned.clone()),
        &home_companion::ha::setup::State::load(
            &cfg.state_dir().join(home_companion::ha::setup::STATE_FILE),
        )
        .ha_addresses,
    );
    let tx = core_tx.clone();
    let server_cfg = wyoming::ServerConfig {
        bind,
        allowed,
        learned: Some(learned.clone()),
        info,
        ping_interval: cfg.satellite.ping.then_some(Duration::from_secs(2)),
        ..Default::default()
    };
    let server = match wyoming::Server::start(
        server_cfg,
        Box::new(move |e| {
            let _ = tx.send(CoreEvent::Server(e));
        }),
    ) {
        Ok(s) => s,
        Err(e) => {
            log::error!(event = "listen_error", error = e.to_string().as_str(); "listening on {bind}: {e}");
            audio.stop();
            return ExitCode::from(exit::SUBSYSTEM);
        }
    };
    log::info!(event = "listening", addr = server.local_addr().to_string().as_str(); "listening on {}", server.local_addr());
    let sender = server.sender();

    // Timers.
    let (timers, loaded) = Timers::load(
        &cfg.state_dir().join("timers.json"),
        cfg.timers.overdue_limit_seconds,
        now_unix(),
    );
    if loaded.kept > 0 || !loaded.dropped.is_empty() {
        log::info!(
            "timers: {} kept, {} overdue dropped",
            loaded.kept,
            loaded.dropped.len()
        );
    }

    // The core.
    let core_cfg = CoreConfig {
        wake_name: model.name.clone(),
        bell_repeats: cfg.timers.bell_repeats,
        bell_gap: Duration::from_secs_f32(cfg.timers.bell_gap_seconds.max(0.0)),
        fps: cfg.ui.fps,
        screenshot_dir: cfg.ui.screenshot_dir.clone().into(),
        sounds_dir: Some(std::path::Path::new(&cfg.character.dir).join("sounds")),
        usage_prices: cfg.usage,
        ..Default::default()
    };
    // Local reflexes: the helper starts now (its model loads in a few seconds) and the core holds
    // utterances back for it only once it is ready.
    let reflex_language = opts.language.clone();
    let reflexes = cfg.reflexes.enabled.then(|| {
        let tx = core_tx.clone();
        let grammar_dir = Path::new(&cfg.character.dir).join("grammar");
        let link = home_companion::reflex::ReflexClient::start(
            home_companion::reflex::HelperPaths {
                helper: home_companion::reflex::helper_path(&cfg.reflexes.helper),
                model_dir: cfg.reflexes.model_dir.clone().into(),
                grammar_dir: grammar_dir.clone(),
            },
            &reflex_language,
            move |e| {
                let _ = tx.send(CoreEvent::Reflex(e));
            },
        );
        home_companion::reflex::Reflexes::new(
            Box::new(link),
            &cfg.reflexes,
            grammar_dir,
            cfg.paths.locales_dir.clone().into(),
            &reflex_language,
        )
    });
    let music = if cfg.airplay.enabled {
        let tx = core_tx.clone();
        match home_companion::airplay::Music::start(
            &cfg.airplay,
            &cfg.satellite.name,
            &cfg.state_dir(),
            player.clone(),
            move |e| {
                let _ = tx.send(CoreEvent::Music(e));
            },
        ) {
            Ok(m) => Some(m),
            Err(e) => {
                log::warn!(event = "airplay_error"; "AirPlay: {e}: off");
                None
            }
        }
    } else {
        None
    };
    // The reflexes' answers in the pipeline's voice (Home Assistant's TTS, cached on disk).
    let voice = (a.state.is_none() && opts.demo.is_none() && !cfg.home_assistant.url.is_empty())
        .then(|| {
            let tx = core_tx.clone();
            home_companion::voice::Voice::start(
                home_companion::voice::VoiceConfig {
                    url: cfg.home_assistant.url.clone(),
                    auth: std::sync::Arc::new(cfg.ha_auth()),
                    state_dir: cfg.state_dir(),
                },
                move |r| {
                    let _ = tx.send(CoreEvent::Voice(r));
                },
            )
            .map_err(|e| log::warn!("voice thread: {e}"))
            .ok()
        })
        .flatten()
        .map(|v| Box::new(v) as Box<dyn home_companion::voice::VoiceLink>);
    let deps = Deps {
        sender,
        player,
        recorder,
        ui: ui_tx,
        mic,
        audio: Some(audio),
        server: Some(server),
        timers,
        alarms: home_companion::alarms::Alarms::load(&cfg.state_dir().join("alarms.json")),
        settings: Some(settings),
        volume: Some(volume),
        wake_stats,
        stop_flag: Some(&signals::STOP),
        reflexes,
        music,
        voice,
        // The turns and the answers to the brain's questions, in order (dropped while the brain
        // is away).
        brain: Some(Box::new(home_companion::brain_link::Ordered::start(
            home_companion::brain_link::socket_path(),
        ))),
    };
    let core = Core::new(core_cfg, deps);
    let core_thread = match std::thread::Builder::new()
        .name("core".into())
        .spawn(move || core.run(core_rx))
    {
        Ok(h) => h,
        Err(e) => {
            log::error!("core thread: {e}");
            return ExitCode::from(exit::SUBSYSTEM);
        }
    };
    if let Some(t) = a.auto_talk_at {
        let tx = core_tx.clone();
        let _ = std::thread::Builder::new()
            .name("auto-talk".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_secs_f64(t));
                let _ = tx.send(CoreEvent::Ui(UiEvent::Talk));
            });
    }

    // The weather from Home Assistant's WebSocket API (its own thread; nothing blocks on it).
    let weather = weather_config(a, cfg, std::sync::Arc::new(cfg.ha_auth())).and_then(|wc| {
        let (tx, rx) = mpsc::channel();
        match home_companion::ha::spawn_weather(wc, tx, signals::stop_requested) {
            Ok(_) => Some(rx),
            Err(e) => {
                log::warn!("weather thread: {e}");
                None
            }
        }
    });

    // The brain (home-companion-brain) talks to Home Assistant with the body's tokens.
    if a.state.is_none() && opts.demo.is_none() && !cfg.home_assistant.url.is_empty() {
        home_companion::brain_link::feed_ha_token(
            home_companion::brain_link::socket_path(),
            std::sync::Arc::new(cfg.ha_auth()),
            cfg.home_assistant.url.clone(),
            cfg.state_dir().join(home_companion::ha::setup::STATE_FILE),
            Settings::in_dir(&cfg.state_dir()),
            cfg.character.language.clone(),
        );
    }

    // Cards and shopping lists from Home Assistant (one WebSocket connection, its own thread):
    // the lists the user chose on the screen, else `[lists] shopping`, else every `todo.*`.
    let settings_lists = Settings::in_dir(&cfg.state_dir()).lists();
    let hub_lists = if !settings_lists.is_empty() {
        settings_lists
    } else if !cfg.lists.shopping.is_empty() {
        vec![cfg.lists.shopping.clone()]
    } else {
        Vec::new()
    };
    let hub = (a.state.is_none() && !cfg.home_assistant.url.is_empty())
        .then(|| {
            let hc = home_companion::ha::hub::HubConfig {
                url: cfg.home_assistant.url.clone(),
                auth: std::sync::Arc::new(cfg.ha_auth()),
                device: character.name.clone(),
                lists: hub_lists.clone(),
            };
            match home_companion::ha::hub::spawn(hc, signals::stop_requested) {
                Ok((rx, tx, _)) => Some((rx, tx, hub_lists)),
                Err(e) => {
                    log::warn!("Home Assistant hub thread: {e}");
                    None
                }
            }
        })
        .flatten();

    // Settings → Home Assistant: the setup after the QR link and its checks (own thread).
    let ha_setup = (a.state.is_none() && opts.demo.is_none() && !cfg.home_assistant.url.is_empty())
        .then(|| {
            let mut sc = cfg
                .setup_config(std::sync::Arc::new(cfg.ha_auth()))
                .map_err(|e| log::warn!("Home Assistant setup: {e}"))
                .ok()?;
            sc.learned = Some(learned.clone());
            home_companion::ha::setup::spawn(sc, signals::stop_requested)
                .map_err(|e| log::warn!("Home Assistant setup thread: {e}"))
                .ok()
        })
        .flatten();

    // The screen, until quit.
    // Without Home Assistant's address (the first start has not found it yet) the view waits.
    let web =
        (cfg.web.enabled && !cfg.home_assistant.url.is_empty() && opts.demo.is_none()).then(|| {
            home_companion::ui::webview::WebOptions {
                cfg: cfg.web.clone(),
                locales: opts.locales.clone(),
                language: opts.language.clone(),
                state_dir: cfg.state_dir(),
                gpu: false,
                page: home_companion::ui::webview::page_url(&cfg.home_assistant.url, &cfg.web.path)
                    .unwrap_or_default(),
            }
        });
    let ctx = UiContext {
        character,
        opts,
        core: core_tx.clone(),
        commands: ui_rx,
        touch,
        wake: wake_sources,
        weather,
        hub,
        ha_setup,
        fps: cfg.ui.fps,
        exit_after: a.exit_after,
        screenshot: a.screenshot.clone(),
        stop_requested: signals::stop_requested,
        web,
    };
    let ui_result = renderer::run(prepared, ctx);
    if let Err(e) = &ui_result {
        log::error!("{e}");
    }
    let _ = core_tx.send(CoreEvent::Quit);
    let summary = match core_thread.join() {
        Ok(s) => s,
        Err(_) => {
            log::error!("the core thread panicked");
            return ExitCode::from(exit::SUBSYSTEM);
        }
    };
    let _ = wake.join();
    log::info!(event = "exit", code = summary.exit_code as i64; "exit {}", summary.exit_code);
    log::logger().flush();
    if summary.exit_code != 0 {
        return ExitCode::from(summary.exit_code as u8);
    }
    if ui_result.is_err() {
        ExitCode::from(exit::RENDERER)
    } else {
        ExitCode::SUCCESS
    }
}

/// The weather thread's settings; none in demo mode (`--state` shows the sample).
fn weather_config(
    a: &args::Args,
    cfg: &Config,
    auth: std::sync::Arc<home_companion::ha::auth::Auth>,
) -> Option<home_companion::ha::WeatherConfig> {
    if a.state.is_some() {
        return None;
    }
    cfg.weather_config(auth)
}
