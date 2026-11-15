// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Updates from the releases of a GitHub repository (Andrea, 2026-11-15): Settings → Info shows
//! the version running, where updates come from and whether a newer one is out; Install updates.
//!
//! - **Checking** is the body's: once a day (and on "Check now") `curl` reads the repository's
//!   releases from GitHub's API (no token), the newest one above this version that has this
//!   architecture's bundle and its `SHA256SUMS` is offered; pre-releases only with
//!   `[update] prerelease = true`.
//! - **Installing** needs root, the body has none: it writes the release's tag to
//!   `<state>/update/request`, and `home-companion-update.path` starts the root service
//!   (`packaging/home-companion-update.sh`). The service takes the repository from the root-owned
//!   configuration only (`[update] repo`), never from the body: a body taken over cannot point it
//!   at other code. It downloads the bundle, checks it against `SHA256SUMS`, keeps the version
//!   running, runs `install.sh --yes`, and puts the old one back if the body does not stay up.
//!   It says how it goes in `<state>/update/status` (`key=value` lines).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// `[update]` in home-companion.toml.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UpdateCfg {
    /// The GitHub repository the releases come from ("owner/name"). The update service reads it
    /// from this file too, as root: change it here only.
    pub repo: String,
    /// Offer pre-releases (release candidates) too.
    pub prerelease: bool,
    /// Look for updates once a day (false: only on "Check now").
    pub check: bool,
}

impl Default for UpdateCfg {
    fn default() -> Self {
        UpdateCfg {
            repo: DEFAULT_REPO.into(),
            prerelease: false,
            check: true,
        }
    }
}

pub const DEFAULT_REPO: &str = "m0s4ik/home-companion";
/// The first check after start, then one a day.
const FIRST_CHECK: Duration = Duration::from_secs(5 * 60);
const EVERY: Duration = Duration::from_secs(24 * 3600);

/// "owner/name" with GitHub's characters only.
pub fn valid_repo(s: &str) -> bool {
    let mut parts = s.split('/');
    let ok = |p: Option<&str>| {
        p.is_some_and(|p| {
            !p.is_empty()
                && p.len() <= 100
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
                && p != "."
                && p != ".."
        })
    };
    ok(parts.next()) && ok(parts.next()) && parts.next().is_none()
}

/// The bundle's architecture name ("arm64" on the ThinkSmart View).
pub fn arch() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        a => a,
    }
}

/// A version: "0.3.1" or "0.3.1-rc2" (a leading "v" is allowed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub nums: [u64; 3],
    /// The pre-release part ("rc2"); None: a release.
    pub pre: Option<String>,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim().trim_start_matches('v');
        let (main, pre) = match s.split_once('-') {
            Some((m, p)) if !p.is_empty() => (m, Some(p.to_string())),
            Some(_) => return None,
            None => (s, None),
        };
        let mut nums = [0u64; 3];
        let mut it = main.split('.');
        for n in nums.iter_mut() {
            *n = it.next()?.parse().ok()?;
        }
        if it.next().is_some() {
            return None;
        }
        Some(Version { nums, pre })
    }

    /// Newer than `other`: by the numbers; for the same numbers a release is newer than its
    /// pre-releases, and pre-releases compare by their text with numbers as numbers (rc10 > rc9).
    pub fn newer_than(&self, other: &Version) -> bool {
        if self.nums != other.nums {
            return self.nums > other.nums;
        }
        match (&self.pre, &other.pre) {
            (None, None) => false,
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (Some(a), Some(b)) => pre_key(a) > pre_key(b),
        }
    }
}

/// "rc10" → ("rc", 10): letters then the number, so rc10 sorts after rc9.
fn pre_key(p: &str) -> (String, u64) {
    let letters: String = p.chars().take_while(|c| !c.is_ascii_digit()).collect();
    let digits: String = p
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (letters, digits.parse().unwrap_or(0))
}

/// A release that can be installed.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub tag: String,
    pub version: String,
    pub prerelease: bool,
    /// The bundle's file name and its SHA-256 from the release's SHA256SUMS (filled after the
    /// check; empty when it could not be read: then nothing is offered to install).
    pub bundle: String,
    pub sha256: String,
    /// The release notes, as plain text for the screen ([`notes_text`]).
    pub notes: String,
}

/// The newest release in GitHub's `releases` list that is newer than `current`, has the bundle
/// for `arch` and a `SHA256SUMS`, is not a draft, and is a pre-release only if allowed.
pub fn pick(releases: &Value, current: &str, allow_pre: bool, arch: &str) -> Option<Release> {
    let cur = Version::parse(current)?;
    let mut best: Option<(Version, Release)> = None;
    for r in releases.as_array()? {
        let tag = r.get("tag_name").and_then(Value::as_str).unwrap_or("");
        let pre = r
            .get("prerelease")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let draft = r.get("draft").and_then(Value::as_bool).unwrap_or(false);
        let Some(v) = Version::parse(tag) else {
            continue;
        };
        if draft || (pre && !allow_pre) || !v.newer_than(&cur) {
            continue;
        }
        if !valid_tag(tag) {
            continue;
        }
        let version = tag.trim_start_matches('v').to_string();
        let bundle = format!("home-companion-{version}-{arch}.tar.gz");
        let names: Vec<&str> = r
            .get("assets")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.get("name").and_then(Value::as_str))
                    .collect()
            })
            .unwrap_or_default();
        if !names.contains(&bundle.as_str()) || !names.contains(&"SHA256SUMS") {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _)| v.newer_than(b)) {
            best = Some((
                v,
                Release {
                    tag: tag.to_string(),
                    version,
                    prerelease: pre,
                    bundle,
                    sha256: String::new(),
                    notes: notes_text(r.get("body").and_then(Value::as_str).unwrap_or("")),
                },
            ));
        }
    }
    best.map(|(_, r)| r)
}

/// The longest notes shown (the rest is on the release's page).
const NOTES_MAX: usize = 4000;

/// GitHub's Markdown release notes as plain text for the screen: no headings' #, no emphasis,
/// links as their text, `> [!NOTE]` markers dropped, list items as "• ", no characters outside
/// the Basic Multilingual Plane (emoji: not in the screen's fonts), blank lines kept single.
pub fn notes_text(md: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for raw in md.lines() {
        let mut l = raw.trim_end().to_string();
        // Quotes and alerts.
        let t = l.trim_start();
        if t.starts_with("> [!") {
            continue;
        }
        if let Some(rest) = t.strip_prefix('>') {
            l = rest.trim_start().to_string();
        }
        let t = l.trim_start();
        let indent = l.len() - t.len();
        let mut line = if let Some(h) = t.strip_prefix('#') {
            h.trim_start_matches('#').trim().to_string()
        } else if let Some(item) = t.strip_prefix("- ").or_else(|| t.strip_prefix("* ")) {
            format!("{}• {item}", "  ".repeat(indent / 2))
        } else {
            t.to_string()
        };
        line = strip_links(&line);
        line = line.replace("**", "").replace("__", "").replace('`', "");
        line.retain(|c| (c as u32) < 0x1_0000 && c != '\u{fe0f}');
        let blank = line.trim().is_empty();
        if blank && out.last().is_none_or(|p| p.is_empty()) {
            continue;
        }
        out.push(if blank { String::new() } else { line });
    }
    while out.last().is_some_and(|l| l.is_empty()) {
        out.pop();
    }
    let mut s = out.join("\n");
    if s.chars().count() > NOTES_MAX {
        s = s.chars().take(NOTES_MAX).collect::<String>() + "…";
    }
    s
}

/// "[text](url)" → "text".
fn strip_links(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find("](") else {
            break;
        };
        let close = open + close;
        let Some(end) = rest[close..].find(')') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&rest[open + 1..close]);
        rest = &rest[close + end + 1..];
    }
    out.push_str(rest);
    out
}

/// A tag the update service accepts (the same rule as its shell check).
pub fn valid_tag(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 40
        && t.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// What the check knows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    /// The last check that worked (seconds since the epoch).
    pub checked_at: Option<i64>,
    /// A newer release, if there is one.
    pub latest: Option<Release>,
    /// The last check failed (why, for the log; the screen says it failed).
    pub failed: bool,
    pub checking: bool,
}

/// The checking thread and what it found.
pub struct Checker {
    state: Arc<Mutex<State>>,
    poke: Sender<()>,
    pub repo: String,
}

impl Checker {
    pub fn spawn(cfg: &UpdateCfg, current: &str) -> Checker {
        let state = Arc::new(Mutex::new(State::default()));
        let (poke, rx) = channel();
        let repo = if valid_repo(&cfg.repo) {
            cfg.repo.clone()
        } else {
            log::warn!(event = "update"; "update: [update] repo {:?} is not owner/name: {DEFAULT_REPO}", cfg.repo);
            DEFAULT_REPO.to_string()
        };
        let (st, r, cur, pre, auto) = (
            state.clone(),
            repo.clone(),
            current.to_string(),
            cfg.prerelease,
            cfg.check,
        );
        let spawned = std::thread::Builder::new()
            .name("update".into())
            .spawn(move || run(st, rx, &r, &cur, pre, auto));
        if let Err(e) = spawned {
            log::warn!(event = "update"; "update thread: {e}");
        }
        Checker { state, poke, repo }
    }

    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// "Check now".
    pub fn check_now(&self) {
        if let Ok(mut s) = self.state.lock() {
            s.checking = true;
        }
        let _ = self.poke.send(());
    }
}

fn run(
    state: Arc<Mutex<State>>,
    poke: Receiver<()>,
    repo: &str,
    current: &str,
    pre: bool,
    auto: bool,
) {
    let mut wait = FIRST_CHECK;
    loop {
        let asked = match poke.recv_timeout(wait) {
            Ok(()) => true,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => false,
            Err(_) => return,
        };
        wait = EVERY;
        if !asked && !auto {
            continue;
        }
        if let Ok(mut s) = state.lock() {
            s.checking = true;
        }
        let got = fetch(repo)
            .map(|v| pick(&v, current, pre, arch()))
            .map(|r| {
                r.map(|mut r| {
                    match fetch_sha256(repo, &r) {
                        Ok(h) => r.sha256 = h,
                        Err(e) => {
                            log::warn!(event = "update"; "update: SHA256SUMS of {}: {e}", r.tag)
                        }
                    }
                    r
                })
            });
        let now = now_unix();
        if let Ok(mut s) = state.lock() {
            s.checking = false;
            match got {
                Ok(latest) => {
                    match &latest {
                        Some(r) => {
                            log::info!(event = "update", available = r.version.as_str(); "update: {} available from {repo}", r.version)
                        }
                        None => {
                            log::info!(event = "update"; "update: {current} is the newest of {repo}")
                        }
                    }
                    s.checked_at = Some(now);
                    s.latest = latest;
                    s.failed = false;
                }
                Err(e) => {
                    log::warn!(event = "update"; "update: checking {repo}: {e}");
                    s.failed = true;
                }
            }
        }
    }
}

/// The repository's releases, newest first (GitHub's API through curl: no TLS in the body).
fn fetch(repo: &str) -> Result<Value, String> {
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=20");
    let out = Command::new("curl")
        .args([
            "-fsS",
            "--max-time",
            "20",
            "-A",
            "home-companion",
            "-H",
            "Accept: application/vnd.github+json",
            &url,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    serde_json::from_slice(&out.stdout).map_err(|e| format!("GitHub's answer: {e}"))
}

/// The bundle's SHA-256 as the release's SHA256SUMS gives it.
fn fetch_sha256(repo: &str, r: &Release) -> Result<String, String> {
    let url = format!(
        "https://github.com/{repo}/releases/download/{}/SHA256SUMS",
        r.tag
    );
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "20", "-A", "home-companion", &url])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    sha_of(&String::from_utf8_lossy(&out.stdout), &r.bundle)
        .ok_or_else(|| format!("no line for {}", r.bundle))
}

/// The hash of `name` in a SHA256SUMS text (`<64 hex>  <name>`, or ` *<name>` in binary mode).
pub fn sha_of(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (h, f) = l.trim().split_once(char::is_whitespace)?;
        let f = f.trim_start().trim_start_matches('*');
        (f == name && valid_sha(h)).then(|| h.to_ascii_lowercase())
    })
}

pub fn valid_sha(h: &str) -> bool {
    h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())
}

/// "d4ede278 2c0b3122 …": eight groups of eight, to read and compare.
pub fn sha_groups(h: &str) -> String {
    h.as_bytes()
        .chunks(8)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// `<state>/update`: the request to the root service and its status.
pub fn dir(state_dir: &Path) -> PathBuf {
    state_dir.join("update")
}

/// Asks the root service to install `tag` whose bundle has the SHA-256 the user saw and
/// confirmed (written whole, then renamed: the path unit sees it only complete). The service
/// installs only a bundle with exactly that hash.
pub fn request(state_dir: &Path, tag: &str, sha256: &str) -> std::io::Result<()> {
    if !valid_tag(tag) {
        return Err(std::io::Error::other("not a release tag"));
    }
    if !valid_sha(sha256) {
        return Err(std::io::Error::other("not a SHA-256"));
    }
    let d = dir(state_dir);
    std::fs::create_dir_all(&d)?;
    let _ = std::fs::remove_file(d.join("status"));
    let tmp = d.join("request.tmp");
    std::fs::write(&tmp, format!("{tag} {}\n", sha256.to_ascii_lowercase()))?;
    std::fs::rename(&tmp, d.join("request"))
}

/// What the root service says: `phase` (downloading, checking, installing, waiting, done,
/// failed, rolled-back), the `version` and a `message`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Status {
    pub phase: String,
    pub version: String,
    pub message: String,
    /// Seconds since the epoch, when it was written.
    pub at: i64,
}

pub fn parse_status(text: &str) -> Option<Status> {
    let mut s = Status::default();
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().to_string();
        match k.trim() {
            "phase" => s.phase = v,
            "version" => s.version = v,
            "message" => s.message = v,
            "at" => s.at = v.parse().unwrap_or(0),
            _ => {}
        }
    }
    (!s.phase.is_empty()).then_some(s)
}

pub fn status(state_dir: &Path) -> Option<Status> {
    std::fs::read_to_string(dir(state_dir).join("status"))
        .ok()
        .and_then(|t| parse_status(&t))
}

/// An install asked and not yet picked up by the service.
pub fn requested(state_dir: &Path) -> bool {
    dir(state_dir).join("request").exists()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn versions() {
        assert_eq!(v("v0.3.0").nums, [0, 3, 0]);
        assert_eq!(v("0.3.0-rc2").pre.as_deref(), Some("rc2"));
        assert!(Version::parse("0.3").is_none());
        assert!(Version::parse("0.3.0.1").is_none());
        assert!(Version::parse("0.3.0-").is_none());
        assert!(Version::parse("latest").is_none());
        assert!(v("0.3.1").newer_than(&v("0.3.0")));
        assert!(v("0.10.0").newer_than(&v("0.9.9")));
        assert!(v("0.3.0").newer_than(&v("0.3.0-rc2")));
        assert!(!v("0.3.0-rc2").newer_than(&v("0.3.0")));
        assert!(v("0.3.0-rc10").newer_than(&v("0.3.0-rc9")));
        assert!(!v("0.3.0").newer_than(&v("0.3.0")));
    }

    fn rel(tag: &str, pre: bool, assets: &[&str]) -> Value {
        json!({"tag_name": tag, "prerelease": pre, "draft": false,
               "assets": assets.iter().map(|a| json!({"name": a})).collect::<Vec<_>>()})
    }

    #[test]
    fn picks_the_newest_installable_release() {
        let list = json!([
            rel(
                "v0.4.0-rc1",
                true,
                &["home-companion-0.4.0-rc1-arm64.tar.gz", "SHA256SUMS"]
            ),
            rel("v0.3.2", false, &["home-companion-0.3.2-arm64.tar.gz"]),
            rel(
                "v0.3.1",
                false,
                &["home-companion-0.3.1-arm64.tar.gz", "SHA256SUMS"]
            ),
            rel(
                "v0.3.0",
                false,
                &["home-companion-0.3.0-arm64.tar.gz", "SHA256SUMS"]
            ),
        ]);
        // 0.3.2 has no SHA256SUMS; the rc only with pre-releases allowed.
        let r = pick(&list, "0.3.0", false, "arm64").unwrap();
        assert_eq!((r.tag.as_str(), r.version.as_str()), ("v0.3.1", "0.3.1"));
        let r = pick(&list, "0.3.0", true, "arm64").unwrap();
        assert_eq!((r.version.as_str(), r.prerelease), ("0.4.0-rc1", true));
        assert_eq!(pick(&list, "0.3.1", false, "arm64"), None);
        // Another architecture: nothing.
        assert_eq!(pick(&list, "0.3.0", true, "amd64"), None);
        // A draft is never offered.
        let d = json!([{"tag_name": "v9.0.0", "prerelease": false, "draft": true,
                        "assets": [{"name": "home-companion-9.0.0-arm64.tar.gz"}, {"name": "SHA256SUMS"}]}]);
        assert_eq!(pick(&d, "0.3.0", true, "arm64"), None);
        // Not a list (an error from GitHub): nothing.
        assert_eq!(
            pick(&json!({"message": "rate limited"}), "0.3.0", true, "arm64"),
            None
        );
    }

    #[test]
    fn a_running_release_candidate_sees_its_release() {
        let list = json!([rel(
            "v0.3.0",
            false,
            &["home-companion-0.3.0-arm64.tar.gz", "SHA256SUMS"]
        )]);
        assert_eq!(
            pick(&list, "0.3.0-rc2", false, "arm64").unwrap().version,
            "0.3.0"
        );
    }

    #[test]
    fn repos_and_tags() {
        assert!(valid_repo("m0s4ik/home-companion"));
        assert!(valid_repo("a.b/c_d"));
        assert!(!valid_repo("m0s4ik"));
        assert!(!valid_repo("a/b/c"));
        assert!(!valid_repo("../etc"));
        assert!(!valid_repo("a/b;rm"));
        assert!(!valid_repo("a/.."));
        assert!(valid_tag("v0.3.1-rc1"));
        assert!(!valid_tag("v0.3.1 x"));
        assert!(!valid_tag("../../x"));
        assert!(!valid_tag(""));
    }

    #[test]
    fn hashes_from_sha256sums() {
        let h = "D4EDE2782C0B31223B00E62A56B95C3620B20992F419A1AE8001BF075BAFC61F";
        let sums = format!("{h}  home-companion-0.3.1-arm64.tar.gz\nffff  other\n");
        assert_eq!(
            sha_of(&sums, "home-companion-0.3.1-arm64.tar.gz"),
            Some(h.to_ascii_lowercase())
        );
        assert_eq!(
            sha_of(&format!("{h} *x.tar.gz"), "x.tar.gz"),
            Some(h.to_ascii_lowercase())
        );
        assert_eq!(sha_of(&sums, "home-companion-0.3.2-arm64.tar.gz"), None);
        assert_eq!(sha_of("ffff  x.tar.gz", "x.tar.gz"), None);
        assert_eq!(
            sha_groups(&h.to_ascii_lowercase()),
            "d4ede278 2c0b3122 3b00e62a 56b95c36 20b20992 f419a1ae 8001bf07 5bafc61f"
        );
    }

    #[test]
    fn notes_for_the_screen() {
        let md = "Second release: **memories** and [German](https://x/4).\n\n> [!IMPORTANT]\n> **Update the system too.**\n\n## New\n\n- **Camera** 📷 tile\n  - nested `code`\n\n\n## Fixed\n";
        assert_eq!(
            notes_text(md),
            "Second release: memories and German.\n\nUpdate the system too.\n\nNew\n\n• Camera  tile\n  • nested code\n\nFixed"
        );
        let long = "x".repeat(5000);
        assert_eq!(notes_text(&long).chars().count(), NOTES_MAX + 1);
    }

    #[test]
    fn status_lines() {
        let s = parse_status("phase=installing\nversion=0.3.1\nmessage=\nat=1794700000\n").unwrap();
        assert_eq!(
            (s.phase.as_str(), s.version.as_str(), s.at),
            ("installing", "0.3.1", 1_794_700_000)
        );
        assert_eq!(parse_status("garbage"), None);
    }

    #[test]
    fn a_request_is_written_whole() {
        let d = std::env::temp_dir().join(format!("hc-update-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let h = "d4ede2782c0b31223b00e62a56b95c3620b20992f419a1ae8001bf075bafc61f";
        request(&d, "v0.3.1", h).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir(&d).join("request")).unwrap(),
            format!("v0.3.1 {h}\n")
        );
        assert!(requested(&d));
        assert!(request(&d, "v0.3.1; rm -rf /", h).is_err());
        assert!(request(&d, "v0.3.1", "abc").is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
