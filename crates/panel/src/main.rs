// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! `home-companion-panel [--socket PATH] [--listen ADDR] [--idle SECONDS]`: the browser panel.
//! Under systemd it takes the listening socket of `home-companion-panel.socket` (`LISTEN_FDS`)
//! and leaves after ten minutes without requests; `--listen 127.0.0.1:8125` is for trying it
//! by hand. `--socket` is the body's (default /run/home-companion/panel/panel.sock).

use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use panel::{Panel, RateLimit, SocketBody};

fn main() -> ExitCode {
    let mut socket = panel::default_socket().to_path_buf();
    let mut listen: Option<String> = None;
    let mut idle = Duration::from_secs(600);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match (a.as_str(), args.next()) {
            ("--socket", Some(v)) => socket = v.into(),
            ("--listen", Some(v)) => listen = Some(v),
            ("--idle", Some(v)) => match v.parse() {
                Ok(s) => idle = Duration::from_secs(s),
                Err(_) => return usage(),
            },
            _ => return usage(),
        }
    }
    let listener = match (panel::systemd_listener(), listen) {
        (Some(l), _) => l,
        (None, Some(addr)) => match std::net::TcpListener::bind(&addr) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("home-companion-panel: {addr}: {e}");
                return ExitCode::from(2);
            }
        },
        (None, None) => {
            eprintln!("home-companion-panel: no socket from systemd and no --listen");
            return usage();
        }
    };
    let p = Arc::new(Panel {
        body: SocketBody { path: socket },
        hostname: panel::hostname(),
        rate: RateLimit::default(),
    });
    match panel::run(p, listener, idle) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("home-companion-panel: {e}");
            ExitCode::from(2)
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: home-companion-panel [--socket PATH] [--listen ADDR] [--idle SECONDS]");
    ExitCode::from(1)
}
