// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! A scripted Home Assistant for testing Wyoming satellites.
//!
//! Exit status: 0 when the scenario passed, 1 when a check failed, 2 on usage or connection
//! errors.

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::process::ExitCode;

use wyoming::fake_ha::{self, BUILTINS};

const USAGE: &str = "usage: fake-ha --connect HOST:PORT [--builtin NAME | --scenario FILE.toml] [--out FILE.json] [--no-ping] [--verbose]
       fake-ha --print NAME      print a built-in scenario as TOML
built-in scenarios: ";

fn usage(msg: &str) -> ExitCode {
    eprintln!("fake-ha: {msg}\n{USAGE}{}", BUILTINS.join(", "));
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let mut connect: Option<String> = None;
    let mut builtin: Option<String> = None;
    let mut scenario_file: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut ping = true;
    let mut verbose = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or_else(|| format!("{name} needs a value"));
        let r = match a.as_str() {
            "--connect" => value("--connect").map(|v| connect = Some(v)),
            "--builtin" => value("--builtin").map(|v| builtin = Some(v)),
            "--scenario" => value("--scenario").map(|v| scenario_file = Some(v.into())),
            "--out" => value("--out").map(|v| out = Some(v.into())),
            "--no-ping" => {
                ping = false;
                Ok(())
            }
            "--verbose" | "-v" => {
                verbose = true;
                Ok(())
            }
            "--print" => {
                let name = match value("--print") {
                    Ok(n) => n,
                    Err(e) => return usage(&e),
                };
                let Some(s) = fake_ha::builtin(&name) else {
                    return usage(&format!("no built-in scenario {name:?}"));
                };
                match toml::to_string_pretty(&s) {
                    Ok(t) => {
                        print!("{t}");
                        return ExitCode::SUCCESS;
                    }
                    Err(e) => {
                        eprintln!("fake-ha: {e}");
                        return ExitCode::from(2);
                    }
                }
            }
            "--help" | "-h" => {
                println!("{USAGE}{}", BUILTINS.join(", "));
                return ExitCode::SUCCESS;
            }
            other => Err(format!("unknown argument {other:?}")),
        };
        if let Err(e) = r {
            return usage(&e);
        }
    }
    wyoming::stub::init_stderr_logger(if verbose {
        log::LevelFilter::Debug
    } else {
        log::LevelFilter::Info
    });

    let Some(connect) = connect else {
        return usage("--connect is required");
    };
    let addr: SocketAddr = match connect.to_socket_addrs().ok().and_then(|mut a| a.next()) {
        Some(a) => a,
        None => return usage(&format!("cannot resolve {connect:?}")),
    };
    let mut scenario = match (builtin, scenario_file) {
        (Some(_), Some(_)) => return usage("--builtin and --scenario are exclusive"),
        (Some(b), None) => match fake_ha::builtin(&b) {
            Some(s) => s,
            None => return usage(&format!("no built-in scenario {b:?}")),
        },
        (None, Some(f)) => match fake_ha::load_scenario(&f) {
            Ok(s) => s,
            Err(e) => return usage(&e),
        },
        (None, None) => return usage("--builtin or --scenario is required"),
    };
    if !ping {
        scenario.ping = false;
    }

    let record = match fake_ha::run(addr, &scenario) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("fake-ha: {addr}: {e}");
            return ExitCode::from(2);
        }
    };
    if let Some(out) = out {
        let json = match serde_json::to_string_pretty(&record) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("fake-ha: {e}");
                return ExitCode::from(2);
            }
        };
        if let Err(e) = std::fs::write(&out, json + "\n") {
            eprintln!("fake-ha: {}: {e}", out.display());
            return ExitCode::from(2);
        }
    }
    eprintln!(
        "fake-ha: {} messages, {:.2} s of audio, detection->first chunk {:?} ms, audio-stop->played {:?} ms",
        record.received.len(),
        record.audio_seconds,
        record.detection_to_first_chunk_ms.map(|v| v.round()),
        record.audio_stop_to_played_ms.map(|v| v.round()),
    );
    if record.failures.is_empty() {
        eprintln!("fake-ha: PASS");
        ExitCode::SUCCESS
    } else {
        for f in &record.failures {
            eprintln!("fake-ha: FAIL: {f}");
        }
        ExitCode::from(1)
    }
}
