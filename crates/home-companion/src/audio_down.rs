// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The audio that does not start: one reboot, then the screen says so.
//!
//! On a ThinkSmart View the audio DSP once stopped answering after a day of running: every
//! command to it timed out (`-110`), the sound card could not be opened again, and only a
//! reboot brought it back. The body cannot reboot by itself (no root, `NoNewPrivileges`), so
//! it exits with [`crate::exit::AUDIO`] when the audio does not start for a reason a reboot
//! may cure ([`exit_for`]; after [`START_TRIES`] tries), and the service's
//! `ExecStopPost` (root) reboots the device for it, at most once: before rebooting it leaves
//! the mark [`MARK`] in the state directory. While the mark is there, a start whose audio
//! fails shows [`SHOWN_FOR`] of a screen saying that the audio is not answering, then exits
//! again: systemd tries once more after that. The body removes the mark once the audio has
//! worked for [`KEEPS_WORKING`], so a later failure may reboot once again.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// The mark, in the state directory: the device was rebooted because the audio did not start.
/// Written by the service (packaging/home-companion.service), removed by the body.
pub const MARK: &str = "audio-reboot";
/// How long the audio must keep working before the mark goes.
pub const KEEPS_WORKING: Duration = Duration::from_secs(10 * 60);
/// How long the screen says that the audio is not answering before the body exits.
pub const SHOWN_FOR: Duration = Duration::from_secs(60);
/// Starts of the audio tried before giving up, and the wait between two: a card slow to come
/// back (after a resume, or a restart of the body) is given ~10 s whatever the uptime.
pub const START_TRIES: u32 = 4;
pub const START_RETRY_AFTER: Duration = Duration::from_secs(3);

/// The exit code for an audio that did not start. [`crate::exit::AUDIO`] (the service reboots
/// for it) only for what a reboot may cure: the device timed out, failed in I/O or is gone
/// (`ETIMEDOUT`, `EIO`, `ENODEV`, `ENXIO`). A wrong device name (`ENOENT`), a device someone
/// else holds (`EBUSY`) and any other error are [`crate::exit::SUBSYSTEM`]; a configuration
/// the audio cannot do is [`crate::exit::DATA`].
pub fn exit_for(e: &audio::Error) -> u8 {
    use crate::exit;
    match e {
        audio::Error::Unsupported(_) => exit::DATA,
        audio::Error::Feature(_) => exit::SUBSYSTEM,
        _ => match e.errno() {
            Some(libc::ETIMEDOUT | libc::EIO | libc::ENODEV | libc::ENXIO) => exit::AUDIO,
            _ => exit::SUBSYSTEM,
        },
    }
}

/// Calls `start` up to `tries` times, `sleep(wait)` between two, while `stop()` is false. A
/// configuration the audio cannot do (`Unsupported`, `Feature`) is not tried again.
pub fn start_with_retries<T>(
    tries: u32,
    wait: Duration,
    mut start: impl FnMut() -> Result<T, audio::Error>,
    mut sleep: impl FnMut(Duration),
    stop: impl Fn() -> bool,
) -> Result<T, audio::Error> {
    let mut tried = 1;
    loop {
        match start() {
            Ok(a) => return Ok(a),
            Err(e @ (audio::Error::Unsupported(_) | audio::Error::Feature(_))) => return Err(e),
            Err(e) if tried >= tries || stop() => return Err(e),
            Err(e) => {
                log::warn!(
                    event = "audio_retry", try = tried, error = e.to_string().as_str();
                    "audio: {e}: trying again in {} s", wait.as_secs()
                );
                sleep(wait);
                tried += 1;
            }
        }
    }
}

pub fn mark(state_dir: &Path) -> PathBuf {
    state_dir.join(MARK)
}

/// The device was already rebooted for the audio (and the audio has not worked since).
pub fn rebooted(state_dir: &Path) -> bool {
    mark(state_dir).exists()
}

/// The audio started: once it has kept working for `after`, the mark goes (when there is
/// one). The body exits on an audio error (core.rs, `AudioEvent::Error`), so a body still
/// running then has working audio.
pub fn clear_after(state_dir: &Path, after: Duration) {
    let path = mark(state_dir);
    if !path.exists() {
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("audio-mark".into())
        .spawn(move || {
            std::thread::sleep(after);
            clear(&path);
        });
    if let Err(e) = spawned {
        log::warn!("audio mark: {e}");
    }
}

fn clear(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {
            log::info!(event = "audio_mark_cleared"; "the audio works again: a later failure may reboot once more")
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => log::warn!("audio mark {}: {e}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hc-audio-down-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn os(errno: i32) -> audio::Error {
        audio::Error::Open {
            what: "playback default".into(),
            source: std::io::Error::from_raw_os_error(errno),
        }
    }

    #[test]
    fn only_what_a_reboot_may_cure_exits_for_a_reboot() {
        use crate::exit;
        for errno in [libc::ETIMEDOUT, libc::EIO, libc::ENODEV, libc::ENXIO] {
            assert_eq!(exit_for(&os(errno)), exit::AUDIO, "errno {errno}");
        }
        // A wrong device name, a busy device, anything else.
        for errno in [libc::ENOENT, libc::EBUSY, libc::EINVAL, libc::EACCES] {
            assert_eq!(exit_for(&os(errno)), exit::SUBSYSTEM, "errno {errno}");
        }
        assert_eq!(
            exit_for(&audio::Error::Unsupported("capture_channels = 3".into())),
            exit::DATA
        );
        assert_eq!(exit_for(&audio::Error::Feature("alsa")), exit::SUBSYSTEM);
        assert_eq!(
            exit_for(&audio::Error::Device("cpal: no device".into())),
            exit::SUBSYSTEM
        );
        let io = audio::Error::Io(std::io::Error::from_raw_os_error(libc::EIO));
        assert_eq!(exit_for(&io), exit::AUDIO);
        let no_errno = audio::Error::Open {
            what: "capture default".into(),
            source: std::io::Error::other("2 channels refused"),
        };
        assert_eq!(exit_for(&no_errno), exit::SUBSYSTEM);
    }

    #[test]
    fn a_card_slow_to_answer_is_given_a_few_tries() {
        let mut calls = 0;
        let mut slept = Vec::new();
        let r = start_with_retries(
            START_TRIES,
            START_RETRY_AFTER,
            || {
                calls += 1;
                if calls <= 2 {
                    Err(os(libc::ETIMEDOUT))
                } else {
                    Ok("opened")
                }
            },
            |d| slept.push(d),
            || false,
        );
        assert_eq!(r.unwrap(), "opened");
        assert_eq!(calls, 3);
        assert_eq!(slept, vec![START_RETRY_AFTER; 2]);
    }

    #[test]
    fn a_card_that_never_answers_gives_up_after_the_tries() {
        let mut calls = 0;
        let r: Result<(), _> = start_with_retries(
            START_TRIES,
            START_RETRY_AFTER,
            || {
                calls += 1;
                Err(os(libc::ETIMEDOUT))
            },
            |_| {},
            || false,
        );
        assert_eq!(exit_for(&r.unwrap_err()), crate::exit::AUDIO);
        assert_eq!(calls, START_TRIES);
        // About ten seconds in all.
        assert!(START_RETRY_AFTER * (START_TRIES - 1) >= Duration::from_secs(9));
    }

    #[test]
    fn a_configuration_mistake_or_a_stop_is_not_tried_again() {
        let mut calls = 0;
        let r: Result<(), _> = start_with_retries(
            START_TRIES,
            START_RETRY_AFTER,
            || {
                calls += 1;
                Err(audio::Error::Unsupported("x".into()))
            },
            |_| {},
            || false,
        );
        assert!(r.is_err());
        assert_eq!(calls, 1);
        let mut calls = 0;
        let r: Result<(), _> = start_with_retries(
            START_TRIES,
            START_RETRY_AFTER,
            || {
                calls += 1;
                Err(os(libc::EIO))
            },
            |_| {},
            || true,
        );
        assert!(r.is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn no_mark_means_not_rebooted_yet() {
        let d = dir("none");
        assert!(!rebooted(&d));
        // Nothing to clear: no thread, nothing written.
        clear_after(&d, Duration::ZERO);
        std::thread::sleep(Duration::from_millis(50));
        assert!(!mark(&d).exists());
    }

    #[test]
    fn the_mark_goes_once_the_audio_kept_working() {
        let d = dir("clear");
        std::fs::write(mark(&d), "").unwrap();
        assert!(rebooted(&d));
        clear_after(&d, Duration::from_millis(100));
        assert!(rebooted(&d), "not before the audio has worked for a while");
        let gone = (0..50).any(|_| {
            std::thread::sleep(Duration::from_millis(20));
            !rebooted(&d)
        });
        assert!(gone);
    }
}
