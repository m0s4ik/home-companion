// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The audio that does not start: one reboot, then the screen says so.
//!
//! On a ThinkSmart View the audio DSP once stopped answering after a day of running: every
//! command to it timed out (`-110`), the sound card could not be opened again, and only a
//! reboot brought it back. The body cannot reboot by itself (no root, `NoNewPrivileges`), so
//! it exits with [`crate::exit::AUDIO`] when the audio does not start, and the service's
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
