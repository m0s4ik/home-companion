// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What "Casa" changes in the system while it is shown or hidden:
//!
//! - the CPU frequency governor (`[web] governor`, "performance"): dashboards open visibly
//!   smoother (Andrea, 2026-11-04); the previous governor of every policy comes back when
//!   the view is hidden, and when the body exits. The body's user needs write access to
//!   `/sys/devices/system/cpu/cpufreq/policy*/scaling_governor` (packaging/home-companion.service gives
//!   it to the group `video` before the body starts);
//! - the helper's cgroup freezer (`cgroup.freeze`, cgroup v2): Home Assistant's frontend keeps
//!   working while hidden (6 % of a core at +1 min), so after a while
//!   hidden the helper's whole cgroup (helper, web and network processes) is frozen, and
//!   thawed before the view is shown again. The cgroup is the one of the process at the other
//!   end of the socket, and only if its name says it is the helper's own (`*home-companion-web*`): never
//!   the body's own cgroup or a login session's.

use std::path::{Path, PathBuf};

const CPUFREQ: &str = "/sys/devices/system/cpu/cpufreq";

fn policies(root: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(root)
        .map(|d| {
            d.flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .is_some_and(|n| n.to_string_lossy().starts_with("policy"))
                })
                .map(|p| p.join("scaling_governor"))
                .filter(|p| p.exists())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Sets a governor while "Casa" is visible and puts the previous ones back.
pub struct Governor {
    want: String,
    root: PathBuf,
    saved: Vec<(PathBuf, String)>,
    warned: bool,
}

impl Governor {
    pub fn new(want: &str) -> Governor {
        Governor::with_root(want, Path::new(CPUFREQ))
    }

    fn with_root(want: &str, root: &Path) -> Governor {
        Governor {
            want: want.trim().to_string(),
            root: root.into(),
            saved: Vec::new(),
            warned: false,
        }
    }

    pub fn boost(&mut self) {
        if self.want.is_empty() || !self.saved.is_empty() {
            return;
        }
        for p in policies(&self.root) {
            let Ok(cur) = std::fs::read_to_string(&p) else {
                continue;
            };
            let cur = cur.trim().to_string();
            if cur == self.want {
                continue;
            }
            match std::fs::write(&p, &self.want) {
                Ok(()) => self.saved.push((p, cur)),
                Err(e) => {
                    if !self.warned {
                        self.warned = true;
                        log::warn!(
                            "web view: governor {} on {}: {e} (packaging/home-companion.service gives the body write access)",
                            self.want,
                            p.display()
                        );
                    }
                }
            }
        }
        if !self.saved.is_empty() {
            log::debug!(
                "web view: governor {} on {} policies",
                self.want,
                self.saved.len()
            );
        }
    }

    pub fn restore(&mut self) {
        for (p, g) in self.saved.drain(..) {
            if let Err(e) = std::fs::write(&p, &g) {
                log::warn!("web view: governor {g} back on {}: {e}", p.display());
            }
        }
    }
}

impl Drop for Governor {
    fn drop(&mut self) {
        self.restore();
    }
}

/// `/proc/<pid>/cgroup` (cgroup v2: "0::/path") → the path.
pub fn cgroup_of(pid: i32) -> Option<String> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    s.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| p.trim().to_string())
}

/// The helper's `cgroup.freeze`, if it may be used: a cgroup of its own named for it.
pub fn freeze_file(helper_pid: i32) -> Result<PathBuf, String> {
    let theirs = cgroup_of(helper_pid).ok_or("helper's cgroup unknown")?;
    let ours = cgroup_of(std::process::id() as i32).unwrap_or_default();
    let leaf = theirs.rsplit('/').next().unwrap_or("");
    if theirs == "/" || theirs == ours || !leaf.contains("home-companion-web") {
        return Err(format!(
            "helper's cgroup {theirs} is not its own (run it as home-companion-web.service): not frozen"
        ));
    }
    let f = PathBuf::from(format!("/sys/fs/cgroup{theirs}/cgroup.freeze"));
    if !f.exists() {
        return Err(format!("{}: missing", f.display()));
    }
    Ok(f)
}

/// Freezes / thaws the helper's cgroup. `frozen` mirrors what was last written.
pub struct Freezer {
    file: Option<PathBuf>,
    frozen: bool,
}

impl Freezer {
    pub fn none() -> Freezer {
        Freezer {
            file: None,
            frozen: false,
        }
    }

    pub fn new(file: PathBuf) -> Freezer {
        // A helper left frozen by a body that died is thawed at once.
        let frozen = std::fs::read_to_string(&file).is_ok_and(|s| s.trim() == "1");
        let mut f = Freezer {
            file: Some(file),
            frozen,
        };
        if frozen {
            log::warn!("web view: the helper was left frozen: thawing it");
            f.thaw();
        }
        f
    }

    pub fn usable(&self) -> bool {
        self.file.is_some()
    }

    pub fn frozen(&self) -> bool {
        self.frozen
    }

    fn write(&mut self, v: &str) -> bool {
        let Some(f) = &self.file else { return false };
        match std::fs::write(f, v) {
            Ok(()) => true,
            Err(e) => {
                log::warn!("web view: {} = {v}: {e} (freezer off)", f.display());
                self.file = None;
                false
            }
        }
    }

    /// Kills the helper's cgroup (`cgroup.kill`, next to the freezer; home-companion-web.service hands it
    /// to the body): systemd starts the helper again (`Restart=on-failure`), and `home-companion web-login`
    /// gives it the tokens of a link made on the screen.
    pub fn kill(&self) -> Result<(), String> {
        let f = self
            .file
            .as_ref()
            .ok_or("the helper's cgroup is unknown")?
            .with_file_name("cgroup.kill");
        std::fs::write(&f, "1").map_err(|e| format!("{}: {e}", f.display()))
    }

    pub fn freeze(&mut self) -> bool {
        if self.frozen || !self.write("1") {
            return false;
        }
        self.frozen = true;
        true
    }

    pub fn thaw(&mut self) {
        if self.frozen && self.write("0") {
            self.frozen = false;
        }
        // Not writable any more: nothing else to do (it stays as it was).
        if self.file.is_none() {
            self.frozen = false;
        }
    }
}

impl Drop for Freezer {
    fn drop(&mut self) {
        self.thaw();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn governor_sets_and_restores() {
        let dir = std::env::temp_dir().join(format!("home-companion-gov-{}", std::process::id()));
        for (p, g) in [("policy0", "schedutil"), ("policy4", "performance")] {
            std::fs::create_dir_all(dir.join(p)).unwrap();
            std::fs::write(dir.join(p).join("scaling_governor"), format!("{g}\n")).unwrap();
        }
        let read = |p: &str| std::fs::read_to_string(dir.join(p).join("scaling_governor")).unwrap();
        {
            let mut g = Governor::with_root("performance", &dir);
            g.boost();
            assert_eq!(read("policy0"), "performance");
            g.restore();
            assert_eq!(read("policy0"), "schedutil");
            assert_eq!(read("policy4").trim(), "performance");
            g.boost();
        }
        // Dropped while boosted: restored.
        assert_eq!(read("policy0"), "schedutil");
        let mut off = Governor::with_root("", &dir);
        off.boost();
        assert_eq!(read("policy0"), "schedutil");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn freezer_writes_and_thaws_on_drop() {
        let f = std::env::temp_dir().join(format!("home-companion-freeze-{}", std::process::id()));
        std::fs::write(&f, "1\n").unwrap();
        {
            let mut z = Freezer::new(f.clone());
            // Left frozen: thawed on creation.
            assert_eq!(std::fs::read_to_string(&f).unwrap(), "0");
            assert!(z.freeze());
            assert!(z.frozen());
            assert_eq!(std::fs::read_to_string(&f).unwrap(), "1");
        }
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "0");
        std::fs::remove_file(&f).unwrap();
    }

    #[test]
    fn own_cgroup_is_never_frozen() {
        assert!(freeze_file(std::process::id() as i32).is_err());
    }
}
