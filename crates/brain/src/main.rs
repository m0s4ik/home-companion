// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! `home-companion-brain`: tools over MCP for Home Assistant's own AI, with
//! long-term memory on the device and no key. (The way with a model called directly and the
//! Wyoming `handle` service is on the branch `standby/brain-direct-llm`.)
//!
//!   home-companion-brain [--config FILE]              serve (default /etc/home-companion/brain.toml)
//!   home-companion-brain tidy [--config FILE]         the nightly tidy-up, now (tests)
//!   home-companion-brain --print-config               the effective configuration

use std::net::{IpAddr, TcpListener, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use brain::agent::Brain;
use brain::config::Config;

struct Logger;

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        // Debug only for our own code (the HTTP and TLS crates are very chatty).
        m.level() <= log::max_level()
            && (m.level() <= log::Level::Info
                || m.target().starts_with("brain")
                || m.target().starts_with("home_companion_brain"))
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            eprintln!(
                "level={} target={} msg={:?}",
                r.level().as_str().to_lowercase(),
                r.target(),
                r.args().to_string()
            );
        }
    }
    fn flush(&self) {}
}

fn main() -> std::process::ExitCode {
    let _ = log::set_logger(&Logger);
    let level = match std::env::var("HOME_COMPANION_BRAIN_LOG").as_deref() {
        Ok("debug") => log::LevelFilter::Debug,
        _ => log::LevelFilter::Info,
    };
    log::set_max_level(level);
    let mut args = std::env::args().skip(1);
    let mut config = PathBuf::from("/etc/home-companion/brain.toml");
    let mut print = false;
    let mut tidy = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config = args.next().map(PathBuf::from).unwrap_or(config),
            "--print-config" => print = true,
            "tidy" => tidy = true,
            "--version" => {
                println!("home-companion-brain {}", env!("CARGO_PKG_VERSION"));
                return 0.into();
            }
            _ => {
                eprintln!("usage: home-companion-brain [tidy] [--config FILE] [--print-config]");
                return 2.into();
            }
        }
    }
    let cfg = match Config::load(&config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("home-companion-brain: {e}");
            return 78.into();
        }
    };
    if print {
        print!("{}", toml::to_string(&cfg).unwrap_or_default());
        return 0.into();
    }
    let brain = match Brain::new(cfg.clone()) {
        Ok(b) => Arc::new(b),
        Err(e) => {
            eprintln!("home-companion-brain: {e}");
            return 1.into();
        }
    };
    if tidy {
        // One tidy-up now (tests: the memory jobs through the configured provider).
        println!("{}", serde_json::to_string(&brain.consolidate()).unwrap_or_default());
        return 0.into();
    }
    let allow: Vec<IpAddr> = cfg
        .allow
        .iter()
        .filter_map(|a| {
            (a.as_str(), 0)
                .to_socket_addrs()
                .ok()?
                .next()
                .map(|s| s.ip())
        })
        .collect();
    // Nobody but Home Assistant: `allow`, or else Home Assistant's own address from its URL.
    let allow = if cfg.allow.is_empty() {
        brain::ha::endpoint(&cfg.home_assistant.url)
            .ok()
            .and_then(|(h, p, _)| (h.as_str(), p).to_socket_addrs().ok()?.next())
            .map(|a| vec![a.ip()])
            .unwrap_or_else(|| vec!["127.0.0.1".parse().unwrap()])
    } else {
        allow
    };
    if !cfg.allow.is_empty() && allow.len() != cfg.allow.len() {
        log::warn!("some `allow` entries do not resolve: only {allow:?}");
    }
    let listener = match TcpListener::bind(&cfg.listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("home-companion-brain: {}: {e}", cfg.listen);
            return 1.into();
        }
    };
    log::info!("connections accepted only from {allow:?}");
    log::info!(
        "home-companion-brain {}: {} on {} (Home Assistant's AI), memory in {}",
        env!("CARGO_PKG_VERSION"),
        brain.name(),
        cfg.listen,
        cfg.memory.dir
    );
    // The body's control socket (the "Ricordi" page, reminder cards).
    let hub = Arc::new(brain::control::Hub::default());
    brain.set_hub(hub.clone());
    if !cfg.control_socket.is_empty()
        && let Err(e) = brain::control::start(brain.clone(), hub.clone(), &cfg.control_socket)
    {
        log::warn!("control socket {}: {e}", cfg.control_socket);
    }
    // Once a minute: conversations quiet for a while (summary and facts), the nightly tidy-up,
    // the morning summary and the reminders. The wall clock is read each time, so after a
    // suspend the next pass catches up (late or missed reminders, the tidy-up within 6 h).
    let (b, h) = (brain.clone(), hub.clone());
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(60));
            b.close_sessions();
            let events = b.tick(brain::memory::now());
            h.broadcast(&events);
        }
    });
    // Every 5 s: unanswered Yes/No questions close after a minute; the calendars are read every
    // 10 minutes (or when the choice changes) and the "next" card gets the new events if they differ.
    let (b, h) = (brain.clone(), hub.clone());
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            b.expire_confirms(brain::agent::now_ms());
            if let Some(ev) = b.refresh_upcoming(brain::memory::now()) {
                h.broadcast(&[ev]);
            }
        }
    });
    // Home Assistant's own AI thinks; the brain serves it tools (MCP).
    log::info!("MCP tools for Home Assistant's conversation agent on http://{}/mcp", cfg.listen);
    brain::mcp::run(brain, listener, allow);
    0.into()
}
