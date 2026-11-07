// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The command line (hand-rolled, as the spikes do).

use std::path::PathBuf;

pub const USAGE: &str =
    "usage: home-companion [--config FILE] [--simulator] [--renderer auto|gpu|software|window|headless]
           [--state home|listening|thinking|speaking|error|offline|ringing|volume] [--screenshot FILE.png]
           [--exit-after SECONDS] [--print-config] [--version] [--reset-console]
       home-companion audio-probe [--config FILE]
       home-companion mic-levels [--seconds N] [--config FILE]
       home-companion wake-test FILE.wav... [--config FILE]
       home-companion check-config [--config FILE] [--simulator]
       home-companion web-login [--config FILE]
       home-companion web-url [--config FILE]
       home-companion ha-setup check|repair [ROW]|unlink [--config FILE]

  --simulator     a window and the laptop's microphone and speaker; Home Assistant may also
                  connect from 127.0.0.1; the repository's characters and locales
  --state S       show state S with fixed sample data (golden screenshots, docs); volume =
                  the home with the volume bar
  --screenshot F  save the screen as PNG at exit (with --exit-after)
  --exit-after N  quit after N seconds
  web-login       print the login line for `home-companion-web --token-stdin` (pipe it, never log it)
  web-url         print the page for `home-companion-web --url` ([home_assistant] url + [web] path);
                  fails while [web] enabled is false
  ha-setup        Home Assistant set up for the body, as Settings → Home Assistant does it:
                  check (read only), repair (what is missing; ROW = also that row's choice),
                  unlink (remove what the body made, then the link)";

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Run,
    AudioProbe,
    MicLevels { seconds: f64 },
    WakeTest { files: Vec<PathBuf> },
    CheckConfig,
    WebLogin,
    WebUrl,
    HaSetup { action: String, row: Option<String> },
    PrintConfig,
    Version,
    ResetConsole,
}

#[derive(Debug, Clone)]
pub struct Args {
    pub command: Command,
    pub config: Option<PathBuf>,
    pub simulator: bool,
    pub renderer: Option<String>,
    pub state: Option<String>,
    pub screenshot: Option<PathBuf>,
    pub exit_after: Option<f64>,
    /// Hidden: press Talk this many seconds after run-satellite (tests without a wake word).
    pub auto_talk_at: Option<f64>,
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        command: Command::Run,
        config: None,
        simulator: false,
        renderer: None,
        state: None,
        screenshot: None,
        exit_after: None,
        auto_talk_at: None,
    };
    let mut it = args.into_iter().peekable();
    let mut seconds = 10.0;
    let mut files = Vec::new();
    let mut sub: Option<String> = None;
    if let Some(first) = it.peek() {
        if !first.starts_with('-') {
            sub = it.next();
        }
    }
    while let Some(flag) = it.next() {
        let mut value = || it.next().ok_or_else(|| format!("{flag} needs a value"));
        let num = |v: String, name: &str| -> Result<f64, String> {
            v.parse::<f64>()
                .ok()
                .filter(|x| x.is_finite() && *x >= 0.0)
                .ok_or_else(|| format!("{name}: not a number: {v}"))
        };
        match flag.as_str() {
            "--config" => a.config = Some(value()?.into()),
            "--simulator" => a.simulator = true,
            "--renderer" => {
                let r = value()?;
                if !["auto", "gpu", "software", "window", "headless"].contains(&r.as_str()) {
                    return Err(format!(
                        "--renderer {r}: auto, gpu, software, window or headless"
                    ));
                }
                a.renderer = Some(r);
            }
            "--state" => {
                let s = value()?;
                if crate::ui::screen::Demo::parse(&s).is_none() {
                    return Err(format!(
                        "--state {s}: home, listening, thinking, speaking, error, offline, ringing or volume"
                    ));
                }
                a.state = Some(s);
            }
            "--screenshot" => a.screenshot = Some(value()?.into()),
            "--exit-after" => a.exit_after = Some(num(value()?, "--exit-after")?),
            "--auto-talk-at" => a.auto_talk_at = Some(num(value()?, "--auto-talk-at")?),
            "--seconds" => seconds = num(value()?, "--seconds")?,
            "--print-config" => a.command = Command::PrintConfig,
            "--version" | "-V" => a.command = Command::Version,
            "--reset-console" => a.command = Command::ResetConsole,
            "--help" | "-h" => return Err(String::new()),
            f if !f.starts_with('-')
                && matches!(sub.as_deref(), Some("wake-test" | "ha-setup")) =>
            {
                files.push(PathBuf::from(f))
            }
            _ => return Err(format!("unknown option {flag}")),
        }
    }
    if let Some(s) = sub {
        a.command = match s.as_str() {
            "audio-probe" => Command::AudioProbe,
            "mic-levels" => Command::MicLevels { seconds },
            "wake-test" if files.is_empty() => {
                return Err("wake-test needs at least one WAV file".into());
            }
            "wake-test" => Command::WakeTest { files },
            "check-config" => Command::CheckConfig,
            "web-login" => Command::WebLogin,
            "web-url" => Command::WebUrl,
            "ha-setup" => {
                let words: Vec<String> = files
                    .iter()
                    .map(|f| f.to_string_lossy().into_owned())
                    .collect();
                match words.as_slice() {
                    [a] if ["check", "repair", "unlink"].contains(&a.as_str()) => {
                        Command::HaSetup {
                            action: a.clone(),
                            row: None,
                        }
                    }
                    [a, r] if a == "repair" && crate::ha::setup::ROWS.contains(&r.as_str()) => {
                        Command::HaSetup {
                            action: a.clone(),
                            row: Some(r.clone()),
                        }
                    }
                    _ => return Err("ha-setup check|repair [ROW]|unlink".into()),
                }
            }
            _ => return Err(format!("unknown command {s}")),
        };
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Result<Args, String> {
        parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn run_options() {
        let a = p("--config x.toml --simulator --state thinking --screenshot o.png --exit-after 2")
            .unwrap();
        assert_eq!(a.command, Command::Run);
        assert!(a.simulator);
        assert_eq!(a.state.as_deref(), Some("thinking"));
        assert_eq!(a.exit_after, Some(2.0));
        assert!(p("--state nap").is_err());
        assert!(p("--renderer vulkan").is_err());
        assert!(p("--bogus").is_err());
    }

    #[test]
    fn subcommands() {
        assert_eq!(
            p("mic-levels --seconds 5").unwrap().command,
            Command::MicLevels { seconds: 5.0 }
        );
        assert_eq!(
            p("wake-test a.wav b.wav --config c.toml").unwrap().command,
            Command::WakeTest {
                files: vec!["a.wav".into(), "b.wav".into()]
            }
        );
        assert!(p("wake-test").is_err());
        assert!(p("frobnicate").is_err());
        assert_eq!(p("check-config").unwrap().command, Command::CheckConfig);
        assert_eq!(p("web-url").unwrap().command, Command::WebUrl);
        assert_eq!(
            p("ha-setup repair vad").unwrap().command,
            Command::HaSetup {
                action: "repair".into(),
                row: Some("vad".into())
            }
        );
        assert!(p("ha-setup repair nothing").is_err());
        assert!(p("ha-setup").is_err());
    }
}
