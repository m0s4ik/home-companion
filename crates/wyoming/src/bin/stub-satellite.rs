// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A minimal Wyoming satellite (see `wyoming::stub`) to try `fake-ha` and as a reference of the
//! expected behaviour. Runs until killed.

use std::process::ExitCode;
use std::time::Duration;

use wyoming::Net;
use wyoming::stub::{Stub, StubConfig};

const USAGE: &str = "usage: stub-satellite [--bind ADDR:PORT] [--allow NET]... [--no-pong] [--no-ping] [--played-early] [--verbose]
(default: --bind 127.0.0.1:10700, allowed 127.0.0.0/8 and ::1)";

fn main() -> ExitCode {
    let mut cfg = StubConfig {
        bind: "127.0.0.1:10700".parse().unwrap_or_else(|_| unreachable!()),
        ..Default::default()
    };
    let mut allowed: Vec<Net> = Vec::new();
    let mut verbose = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--bind" => match args.next().and_then(|v| v.parse().ok()) {
                Some(b) => cfg.bind = b,
                None => return fail("--bind needs ADDR:PORT"),
            },
            "--allow" => match args.next().map(|v| Net::parse(&v)) {
                Some(Ok(n)) => allowed.push(n),
                Some(Err(e)) => return fail(&e),
                None => return fail("--allow needs a value"),
            },
            "--no-pong" => cfg.answer_pings = false,
            "--no-ping" => cfg.ping = false,
            "--played-early" => cfg.played_early = true,
            "--verbose" | "-v" => verbose = true,
            "--help" | "-h" => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => return fail(&format!("unknown argument {other:?}")),
        }
    }
    if !allowed.is_empty() {
        cfg.allowed = allowed;
    }
    wyoming::stub::init_stderr_logger(if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });
    let stub = match Stub::start(cfg) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("stub-satellite: {e}");
            return ExitCode::from(2);
        }
    };
    eprintln!("stub-satellite: listening on {}", stub.local_addr());
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn fail(msg: &str) -> ExitCode {
    eprintln!("stub-satellite: {msg}\n{USAGE}");
    ExitCode::from(2)
}
