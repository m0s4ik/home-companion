// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Scenes to download (Settings → Scenes): pictures of the character doing something, which
//! Home Assistant's cards show with `pose: <name>` (README, "From your automations").
//!
//! - **Where they come from**: the newest release of `[update] repo` (the same root-owned
//!   setting as the updates, never the screen's) whose tag starts with `scenes-` and has a
//!   `scenes.json` among its files: the catalogue ([`parse_catalogue`]). Each scene is a few
//!   files of that release (the picture, and an animation table and sheet if it moves), each
//!   with its size and SHA-256 in the catalogue.
//! - **Where they go**: `<state_dir>/scenes/<character>/`, where the body looks first
//!   (`Ui::scene_dirs`): the body's own directory, no root needed, kept by updates. A scene is
//!   downloaded into a temporary directory, every file checked against its size and hash, and
//!   only then moved in; removing takes its files away.
//! - **How**: `curl` (no TLS in the body, as the updates) and `sha256sum`, in a thread of its own
//!   ([`spawn`]); the screen asks ([`Cmd`]) and is told ([`Msg`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender, channel};

use serde::Deserialize;
use serde_json::Value;

/// The file every catalogue release has.
pub const CATALOGUE: &str = "scenes.json";
/// The tags of the catalogue's releases.
pub const TAG_PREFIX: &str = "scenes-";
/// A scene's file is at most this big (a sheet of 80 frames is ~8 MB).
const MAX_FILE: u64 = 32 * 1024 * 1024;

/// One file of a scene: `file` in the scene directory, from the release's `url`.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneFile {
    pub file: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
}

/// A scene of the catalogue.
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    pub character: String,
    pub name: String,
    /// Language code → title ("en" is the fallback).
    pub titles: BTreeMap<String, String>,
    pub files: Vec<SceneFile>,
}

impl Scene {
    /// Its title in `lang`, else English, else its name.
    pub fn title(&self, lang: &str) -> String {
        self.titles
            .get(lang)
            .or_else(|| self.titles.get("en"))
            .cloned()
            .unwrap_or_else(|| self.name.clone())
    }

    /// The size of all its files.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }
}

/// The newest release of the catalogue in GitHub's list of releases (newest first): its tag and
/// its files (name → download URL). Drafts and pre-releases are left out.
pub fn pick_release(releases: &Value) -> Option<(String, BTreeMap<String, String>)> {
    releases.as_array()?.iter().find_map(|r| {
        let tag = r.get("tag_name")?.as_str()?;
        if !tag.starts_with(TAG_PREFIX)
            || r.get("draft").and_then(Value::as_bool) == Some(true)
            || r.get("prerelease").and_then(Value::as_bool) == Some(true)
        {
            return None;
        }
        let assets: BTreeMap<String, String> = r
            .get("assets")?
            .as_array()?
            .iter()
            .filter_map(|a| {
                Some((
                    a.get("name")?.as_str()?.to_string(),
                    a.get("browser_download_url")?.as_str()?.to_string(),
                ))
            })
            .collect();
        assets
            .contains_key(CATALOGUE)
            .then(|| (tag.to_string(), assets))
    })
}

/// A scene's name: lowercase letters, digits, `-` and `_` (as `pose` of a card).
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 40
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// The files a scene `name` may have: its picture, its animation table and its sheet.
pub fn valid_file(name: &str, file: &str) -> bool {
    [
        format!("{name}.png"),
        format!("{name}.toml"),
        format!("{name}-anim.png"),
    ]
    .iter()
    .any(|f| f == file)
}

/// The catalogue (`scenes.json`) of a release whose files are `assets`. Entries that are not
/// well formed are left out, with the reason in the second list.
///
/// ```json
/// {"format": 1, "scenes": [{"character": "alfred", "name": "laundry",
///   "title": {"en": "Laundry", "nl": "Was"},
///   "files": [{"file": "laundry.png", "asset": "alfred-laundry.png", "size": 412345,
///              "sha256": "0f8b…"}]}]}
/// ```
pub fn parse_catalogue(
    text: &str,
    assets: &BTreeMap<String, String>,
) -> Result<(Vec<Scene>, Vec<String>), String> {
    #[derive(Deserialize)]
    struct RawFile {
        file: String,
        asset: String,
        size: u64,
        sha256: String,
    }
    #[derive(Deserialize)]
    struct RawScene {
        character: String,
        name: String,
        #[serde(default)]
        title: BTreeMap<String, String>,
        files: Vec<RawFile>,
    }
    #[derive(Deserialize)]
    struct Raw {
        format: u32,
        scenes: Vec<RawScene>,
    }
    let raw: Raw = serde_json::from_str(text).map_err(|e| format!("{CATALOGUE}: {e}"))?;
    if raw.format != 1 {
        return Err(format!(
            "{CATALOGUE}: format {} (this body reads 1)",
            raw.format
        ));
    }
    let mut scenes = Vec::new();
    let mut skipped = Vec::new();
    for s in raw.scenes {
        let what = format!("{}/{}", s.character, s.name);
        let picture = format!("{}.png", s.name);
        let problem = if !valid_name(&s.character) || !valid_name(&s.name) {
            Some("not a plain name".to_string())
        } else if !s.files.iter().any(|f| f.file == picture) {
            Some(format!("no {picture}"))
        } else {
            s.files.iter().find_map(|f| {
                if !valid_file(&s.name, &f.file) {
                    Some(format!("{}: not one of its files", f.file))
                } else if !assets.contains_key(&f.asset) {
                    Some(format!("{}: not in the release", f.asset))
                } else if !crate::update::valid_sha(&f.sha256) {
                    Some(format!("{}: not a SHA-256", f.file))
                } else if f.size == 0 || f.size > MAX_FILE {
                    Some(format!("{}: {} bytes", f.file, f.size))
                } else {
                    None
                }
            })
        };
        if let Some(p) = problem {
            skipped.push(format!("{what}: {p}"));
            continue;
        }
        scenes.push(Scene {
            files: s
                .files
                .into_iter()
                .map(|f| SceneFile {
                    url: assets[&f.asset].clone(),
                    file: f.file,
                    sha256: f.sha256.to_ascii_lowercase(),
                    size: f.size,
                })
                .collect(),
            character: s.character,
            name: s.name,
            titles: s.title,
        });
    }
    Ok((scenes, skipped))
}

/// Whether the scene is in `dir` (its picture is).
pub fn installed(dir: &Path, name: &str) -> bool {
    dir.join(format!("{name}.png")).is_file()
}

/// The scenes in `dir` that the catalogue no longer lists (`known`: its names), with the size of
/// their files: shown so they can still be taken away.
pub fn unlisted(dir: &Path, known: &[&str]) -> Vec<(String, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, u64)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().to_string();
            let name = file.strip_suffix(".png")?;
            (!name.ends_with("-anim") && valid_name(name) && !known.contains(&name))
                .then(|| name.to_string())
        })
        .map(|name| {
            let size = [
                format!("{name}.png"),
                format!("{name}.toml"),
                format!("{name}-anim.png"),
            ]
            .iter()
            .filter_map(|f| std::fs::metadata(dir.join(f)).ok())
            .map(|m| m.len())
            .sum();
            (name, size)
        })
        .collect();
    out.sort();
    out
}

/// Puts `scene` in `dir`: every file fetched into a temporary directory next to it by `fetch(url,
/// path)`, checked against its size and `hash(path)`, then moved in (the picture last, so a scene
/// is never half there). Nothing of it stays when a file fails.
pub fn install(
    dir: &Path,
    scene: &Scene,
    fetch: impl Fn(&str, &Path) -> Result<(), String>,
    hash: impl Fn(&Path) -> Result<String, String>,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(format!(".download-{}", scene.name));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let result = (|| {
        for f in &scene.files {
            let path = tmp.join(&f.file);
            fetch(&f.url, &path)?;
            let size = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
            if size != f.size {
                return Err(format!("{}: {size} bytes instead of {}", f.file, f.size));
            }
            let h = hash(&path)?;
            if !h.eq_ignore_ascii_case(&f.sha256) {
                return Err(format!("{}: SHA-256 does not match", f.file));
            }
        }
        let picture = format!("{}.png", scene.name);
        let mut order: Vec<&SceneFile> = scene.files.iter().filter(|f| f.file != picture).collect();
        order.extend(scene.files.iter().filter(|f| f.file == picture));
        for f in order {
            std::fs::rename(tmp.join(&f.file), dir.join(&f.file))
                .map_err(|e| format!("{}: {e}", f.file))?;
        }
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    result
}

/// Takes the scene `name` out of `dir` (the picture first: it is no scene from then on).
pub fn remove(dir: &Path, name: &str) -> Result<(), String> {
    for f in [
        format!("{name}.png"),
        format!("{name}.toml"),
        format!("{name}-anim.png"),
    ] {
        match std::fs::remove_file(dir.join(&f)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{f}: {e}")),
        }
    }
    Ok(())
}

/// What the screen asks.
#[derive(Debug, Clone, PartialEq)]
pub enum Cmd {
    /// Read the catalogue again.
    Refresh,
    Install(String),
    Remove(String),
}

/// What the screen is told.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    /// The character's scenes of the catalogue, or why there are none.
    Catalogue(Result<Vec<Scene>, String>),
    /// Working on this scene (None: done).
    Busy(Option<String>),
    /// A scene came or went: the screen shows it again if a card wants it.
    Changed(String),
    /// Installing or removing failed.
    Failed(String, String),
}

/// The thread that reads the catalogue of `repo` and installs the scenes of `character` in
/// `dir`. It waits for the screen's first `Refresh` (when Settings → Scenes opens).
pub fn spawn(repo: String, character: String, dir: PathBuf) -> (Sender<Cmd>, Receiver<Msg>) {
    let (cmd_tx, cmd_rx) = channel::<Cmd>();
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new()
        .name("scenes".into())
        .spawn(move || {
            let mut known: Vec<Scene> = Vec::new();
            for cmd in cmd_rx {
                match cmd {
                    Cmd::Refresh => {
                        let got = catalogue(&repo).map(|(scenes, skipped)| {
                            for s in skipped {
                                log::warn!(event = "scenes"; "scenes: left out {s}");
                            }
                            scenes
                                .into_iter()
                                .filter(|s| s.character == character)
                                .collect::<Vec<_>>()
                        });
                        match &got {
                            Ok(s) => log::info!(event = "scenes"; "scenes: {} for {character} in {repo}", s.len()),
                            Err(e) => log::warn!(event = "scenes"; "scenes: catalogue of {repo}: {e}"),
                        }
                        if let Ok(s) = &got {
                            known = s.clone();
                        }
                        if tx.send(Msg::Catalogue(got)).is_err() {
                            return;
                        }
                    }
                    Cmd::Install(name) | Cmd::Remove(name)
                        if !known.iter().any(|s| s.name == name) && !installed(&dir, &name) =>
                    {
                        log::warn!(event = "scenes"; "scenes: {name} is not in the catalogue");
                    }
                    Cmd::Install(name) => {
                        let Some(scene) = known.iter().find(|s| s.name == name).cloned() else {
                            continue;
                        };
                        let _ = tx.send(Msg::Busy(Some(name.clone())));
                        match install(&dir, &scene, download, sha256) {
                            Ok(()) => {
                                log::info!(event = "scenes"; "scenes: {name} installed ({} bytes)", scene.size());
                                let _ = tx.send(Msg::Changed(name));
                            }
                            Err(e) => {
                                log::warn!(event = "scenes"; "scenes: installing {name}: {e}");
                                let _ = tx.send(Msg::Failed(name, e));
                            }
                        }
                        let _ = tx.send(Msg::Busy(None));
                    }
                    Cmd::Remove(name) => {
                        match remove(&dir, &name) {
                            Ok(()) => {
                                log::info!(event = "scenes"; "scenes: {name} removed");
                                let _ = tx.send(Msg::Changed(name));
                            }
                            Err(e) => {
                                log::warn!(event = "scenes"; "scenes: removing {name}: {e}");
                                let _ = tx.send(Msg::Failed(name, e));
                            }
                        }
                    }
                }
            }
        });
    (cmd_tx, rx)
}

/// The catalogue of `repo`: its newest `scenes-` release and its `scenes.json`.
fn catalogue(repo: &str) -> Result<(Vec<Scene>, Vec<String>), String> {
    let releases = crate::update::fetch(repo)?;
    let (tag, assets) = pick_release(&releases)
        .ok_or_else(|| format!("no {TAG_PREFIX}… release with {CATALOGUE}"))?;
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "20",
            "-A",
            "home-companion",
            &assets[CATALOGUE],
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    log::info!(event = "scenes"; "scenes: catalogue {tag}");
    parse_catalogue(&String::from_utf8_lossy(&out.stdout), &assets)
}

/// A release's file to `path` (curl follows GitHub's redirect to its storage).
fn download(url: &str, path: &Path) -> Result<(), String> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "300", "-A", "home-companion", "-o"])
        .arg(path)
        .arg(url)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// The SHA-256 of a file (coreutils' sha256sum).
fn sha256(path: &Path) -> Result<String, String> {
    let out = Command::new("sha256sum")
        .arg(path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("sha256sum: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| "sha256sum: no hash".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SHA: &str = "0f8bf0a89bc710f845793ca84047fae5e2dcfd87d09c4e7681f83ca386161f75";

    fn assets() -> BTreeMap<String, String> {
        ["scenes.json", "alfred-laundry.png", "alfred-laundry.toml"]
            .iter()
            .map(|a| (a.to_string(), format!("https://x/{a}")))
            .collect()
    }

    #[test]
    fn the_newest_scenes_release_with_a_catalogue() {
        let releases = json!([
            {"tag_name": "v0.3.1", "assets": [{"name": "scenes.json", "browser_download_url": "u"}]},
            {"tag_name": "scenes-v3", "draft": true, "assets": [{"name": "scenes.json", "browser_download_url": "u"}]},
            {"tag_name": "scenes-v2", "assets": [{"name": "other", "browser_download_url": "u"}]},
            {"tag_name": "scenes-v1", "assets": [{"name": "scenes.json", "browser_download_url": "c"},
                                                  {"name": "alfred-laundry.png", "browser_download_url": "p"}]}
        ]);
        let (tag, a) = pick_release(&releases).unwrap();
        assert_eq!(tag, "scenes-v1");
        assert_eq!(a["alfred-laundry.png"], "p");
        assert_eq!(pick_release(&json!([])), None);
    }

    #[test]
    fn a_catalogue_and_what_it_leaves_out() {
        let text = json!({"format": 1, "scenes": [
            {"character": "alfred", "name": "laundry", "title": {"en": "Laundry", "nl": "Was"},
             "files": [{"file": "laundry.png", "asset": "alfred-laundry.png", "size": 10, "sha256": SHA},
                       {"file": "laundry.toml", "asset": "alfred-laundry.toml", "size": 5, "sha256": SHA}]},
            {"character": "alfred", "name": "../x", "files": []},
            {"character": "alfred", "name": "vacuum",
             "files": [{"file": "vacuum.png", "asset": "missing.png", "size": 10, "sha256": SHA}]},
            {"character": "alfred", "name": "waste",
             "files": [{"file": "idle.png", "asset": "alfred-laundry.png", "size": 10, "sha256": SHA}]}
        ]})
        .to_string();
        let (scenes, skipped) = parse_catalogue(&text, &assets()).unwrap();
        assert_eq!(scenes.len(), 1);
        let s = &scenes[0];
        assert_eq!(
            (s.title("nl"), s.title("de"), s.size()),
            ("Was".into(), "Laundry".into(), 15)
        );
        assert_eq!(s.files[0].url, "https://x/alfred-laundry.png");
        assert_eq!(skipped.len(), 3, "{skipped:?}");
        assert!(parse_catalogue(r#"{"format": 2, "scenes": []}"#, &assets()).is_err());
    }

    #[test]
    fn only_the_files_of_the_scene() {
        assert!(valid_file("laundry", "laundry.png"));
        assert!(valid_file("laundry", "laundry.toml"));
        assert!(valid_file("laundry", "laundry-anim.png"));
        for f in ["idle.png", "../laundry.png", "laundry.txt", "vacuum.png"] {
            assert!(!valid_file("laundry", f), "{f}");
        }
        assert!(valid_name("bbq-2") && !valid_name("Bbq") && !valid_name("a/b") && !valid_name(""));
    }

    fn scene(sha: &str) -> Scene {
        Scene {
            character: "alfred".into(),
            name: "laundry".into(),
            titles: BTreeMap::new(),
            files: vec![
                SceneFile {
                    file: "laundry.png".into(),
                    url: "p".into(),
                    sha256: sha.into(),
                    size: 3,
                },
                SceneFile {
                    file: "laundry.toml".into(),
                    url: "t".into(),
                    sha256: sha.into(),
                    size: 3,
                },
            ],
        }
    }

    #[test]
    fn scenes_the_catalogue_no_longer_lists() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-unlisted-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, n) in [
            ("laundry_done.png", 3),
            ("washing.png", 2),
            ("washing.toml", 1),
            ("washing-anim.png", 4),
            ("Bad Name.png", 1),
            (".download-x", 1),
        ] {
            std::fs::write(dir.join(f), vec![0u8; n]).unwrap();
        }
        assert_eq!(
            unlisted(&dir, &["laundry_done"]),
            vec![("washing".to_string(), 7)]
        );
        assert!(unlisted(&dir.join("missing"), &[]).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn install_checks_every_file_and_remove_takes_them_away() {
        let dir =
            std::env::temp_dir().join(format!("home-companion-scene-dl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let fetch = |_: &str, p: &Path| std::fs::write(p, b"abc").map_err(|e| e.to_string());
        let hash = |_: &Path| Ok::<_, String>(SHA.to_string());
        install(&dir, &scene(SHA), fetch, hash).unwrap();
        assert!(installed(&dir, "laundry"));
        assert!(dir.join("laundry.toml").is_file());
        assert!(!dir.join(".download-laundry").exists());
        remove(&dir, "laundry").unwrap();
        assert!(!installed(&dir, "laundry") && !dir.join("laundry.toml").exists());
        // A wrong hash: nothing of the scene stays.
        let other = "1".repeat(64);
        assert!(install(&dir, &scene(&other), fetch, hash).is_err());
        assert!(!installed(&dir, "laundry") && !dir.join("laundry.toml").exists());
        // A wrong size: the same.
        let short = |_: &str, p: &Path| std::fs::write(p, b"ab").map_err(|e| e.to_string());
        assert!(install(&dir, &scene(SHA), short, hash).is_err());
        assert!(!installed(&dir, "laundry"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
