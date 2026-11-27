// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! packaging/home-companion-audio-reboot.sh (the service's reboot for an audio that does not
//! start) against a fake `systemctl`, a fake uptime and a mark in a scratch directory.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/home-companion-audio-reboot.sh")
}

/// One run's world: a scratch directory with the fake `systemctl` first in `PATH`.
struct World {
    dir: PathBuf,
}

impl World {
    fn new(name: &str) -> World {
        let dir =
            std::env::temp_dir().join(format!("hc-audio-reboot-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        // `systemctl show …` prints $FAKE_UPDATE_STATE; any other call is logged, and fails
        // when $FAKE_REBOOT_FAILS is set.
        let fake = "#!/bin/sh\n\
            if [ \"$1\" = show ]; then echo \"$FAKE_UPDATE_STATE\"; exit 0; fi\n\
            echo \"$*\" >> \"$FAKE_LOG\"\n\
            [ -z \"${FAKE_REBOOT_FAILS:-}\" ]\n";
        let bin = dir.join("bin/systemctl");
        std::fs::write(&bin, fake).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        World { dir }
    }

    fn mark(&self) -> PathBuf {
        self.dir.join("audio-reboot")
    }

    /// Runs the script: the body's exit status, the uptime in seconds, the updater's state.
    /// Returns the calls to `systemctl` other than `show`.
    fn run(&self, status: &str, uptime: u32, update: &str, reboot_fails: bool) -> Vec<String> {
        let uptime_file = self.dir.join("uptime");
        std::fs::write(&uptime_file, format!("{uptime}.42 1234.56\n")).unwrap();
        let log = self.dir.join("calls");
        let _ = std::fs::remove_file(&log);
        let path = format!(
            "{}:{}",
            self.dir.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let mut cmd = Command::new("sh");
        cmd.arg(script())
            .arg(self.mark())
            .env("PATH", path)
            .env("EXIT_STATUS", status)
            .env("HC_UPTIME_FILE", &uptime_file)
            .env("FAKE_UPDATE_STATE", update)
            .env("FAKE_LOG", &log);
        if reboot_fails {
            cmd.env("FAKE_REBOOT_FAILS", "1");
        }
        let out = cmd.output().unwrap();
        assert!(out.status.success(), "always exits 0: {out:?}");
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

const REBOOT: &str = "--no-block reboot";

#[test]
fn exit_six_reboots_once_and_leaves_the_mark() {
    let w = World::new("six");
    assert_eq!(w.run("6", 400, "inactive", false), vec![REBOOT]);
    let meta = std::fs::symlink_metadata(w.mark()).unwrap();
    assert!(meta.is_file(), "a plain file");
    // No leftover of the temporary file.
    let left: Vec<_> = std::fs::read_dir(&w.dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("audio-reboot."))
        .collect();
    assert!(left.is_empty(), "{left:?}");
    // The mark is there: not again.
    assert!(w.run("6", 400, "inactive", false).is_empty());
}

#[test]
fn any_other_exit_status_does_nothing() {
    let w = World::new("other");
    for status in ["0", "1", "2", "4", "60", ""] {
        assert!(w.run(status, 400, "inactive", false).is_empty(), "{status}");
        assert!(!w.mark().exists(), "{status}");
    }
}

#[test]
fn not_within_five_minutes_of_booting() {
    let w = World::new("boot");
    assert!(w.run("6", 299, "inactive", false).is_empty());
    assert!(!w.mark().exists());
    assert_eq!(w.run("6", 300, "inactive", false), vec![REBOOT]);
}

#[test]
fn never_while_an_update_runs() {
    let w = World::new("update");
    for state in ["activating", "active", "deactivating", "reloading"] {
        assert!(w.run("6", 400, state, false).is_empty(), "{state}");
        assert!(!w.mark().exists(), "{state}");
    }
    assert_eq!(w.run("6", 400, "failed", false), vec![REBOOT]);
}

#[test]
fn a_link_at_the_mark_is_never_followed() {
    let w = World::new("link");
    let target = w.dir.join("elsewhere");
    std::os::unix::fs::symlink(&target, w.mark()).unwrap();
    assert!(w.run("6", 400, "inactive", false).is_empty());
    assert!(!target.exists(), "the link's target was not made");
    assert!(
        std::fs::symlink_metadata(w.mark())
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn a_refused_reboot_takes_the_mark_back() {
    let w = World::new("refused");
    assert_eq!(w.run("6", 400, "inactive", true), vec![REBOOT]);
    assert!(
        !w.mark().exists(),
        "no mark for a reboot that did not happen"
    );
}
