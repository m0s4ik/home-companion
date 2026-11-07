// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Command line (hand-rolled, like spike-ui: one less dependency on the device).

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Slint linuxkms backend + femtovg (OpenGL ES via EGL/GBM, page flipping).
    Gpu,
    /// Slint winit backend + femtovg in a desktop window (build machine only:
    /// exercises the femtovg and texture code under Xvfb/llvmpipe).
    GpuWindow,
    /// Pre-scaled BGRX frames copied straight into /dev/fb0; Slint software only for the clock.
    SoftBlit,
    /// Slint software renderer draws the frames (our fbdev platform, render into RAM).
    SoftSlint,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Gpu => "gpu",
            Mode::GpuWindow => "gpu-window",
            Mode::SoftBlit => "soft-blit",
            Mode::SoftSlint => "soft-slint",
        }
    }

    pub fn is_gpu(self) -> bool {
        matches!(self, Mode::Gpu | Mode::GpuWindow)
    }
}

/// How the GPU modes get the frames onto the GPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Textures {
    /// Our own GL textures, uploaded once at the first frame and handed to
    /// Slint as borrowed textures: nothing is uploaded while playing.
    Gl,
    /// Plain Slint images (RGB8 in RAM): femtovg uploads a new texture each
    /// time the image changes (its texture cache has no key for buffers).
    Upload,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub xres: u32,
    pub yres: u32,
    pub line_length: u32,
}

#[derive(Clone, Debug)]
pub struct Args {
    pub mode: Mode,
    pub frames_dir: PathBuf,
    /// Pose folder names; empty = every folder of `frames_dir`.
    pub poses: Vec<String>,
    pub fps: f64,
    pub pose_every: f64,
    pub char_scale: f64,
    pub textures: Textures,
    pub seconds: f64,
    pub fb: PathBuf,
    pub fb_geometry: Option<Geometry>,
    pub screenshot: Option<PathBuf>,
    pub stats: Option<PathBuf>,
    pub no_vt: bool,
    pub verbose: bool,
    pub reset_console: bool,
}

pub const USAGE: &str = "\
usage: spike-gpu --mode gpu|soft-blit|soft-slint [options]
  --mode M                  gpu: Slint linuxkms + femtovg (OpenGL ES, EGL/GBM, page flips)
                            soft-blit: frames pre-scaled to BGRX, copied into /dev/fb0; Slint software for the clock
                            soft-slint: Slint software renderer draws the frames (own fbdev platform, render into RAM)
                            gpu-window: femtovg in a desktop window (build machine only, feature gpu-window)
  --frames-dir DIR          animation folder with one sub-folder per pose of frame_NNN.png
                            (default fixtures/generated/alfred-animations next to the binary's crate, else ./alfred-animations)
  --poses A,B,...           pose folders to load and cycle, or 'all' (default idle,listening)
  --fps N                   animation and tick rate (default 16)
  --pose-every S            switch to the next pose every S seconds, 0 = never (default 8)
  --char-scale K            on-screen size of the character = K x frame size, 0.25..1.31 (default 1.0: 440x760)
  --textures gl|upload      gpu: own GL textures uploaded once (default), or Slint images re-uploaded on change
  --seconds N               run for N seconds of scene time, then exit cleanly (default 30)
  --fb PATH                 soft modes: framebuffer device or plain file (default /dev/fb0)
  --fb-geometry WxHxLINE    geometry when the FBIOGET ioctls fail (plain file), e.g. 800x1280x3200
  --screenshot FILE.png     screenshot at exit
  --stats FILE.json         statistics as JSON at exit
  --no-vt                   do not switch the text console to graphics mode
  --verbose                 one line per frame on stderr
  --reset-console           only set /dev/tty0 back to text mode and exit
";

fn num<T: std::str::FromStr>(flag: &str, v: &str) -> Result<T, String> {
    v.parse().map_err(|_| format!("{flag}: not a number: {v}"))
}

fn parse_geometry(v: &str) -> Result<Geometry, String> {
    let p: Vec<&str> = v.split('x').collect();
    if p.len() != 3 {
        return Err(format!("--fb-geometry: expected WxHxLINE_LENGTH, got {v}"));
    }
    let g = Geometry {
        xres: num("--fb-geometry", p[0])?,
        yres: num("--fb-geometry", p[1])?,
        line_length: num("--fb-geometry", p[2])?,
    };
    if g.xres == 0 || g.yres == 0 || g.line_length < g.xres * 4 {
        return Err(format!("--fb-geometry: invalid for 32 bpp: {v}"));
    }
    Ok(g)
}

/// The default animation folder: the crate's git-ignored fixtures when run
/// from the source tree, else `alfred-animations` in the current directory
/// (the device layout of the README).
fn default_frames_dir() -> PathBuf {
    let in_tree = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/generated/alfred-animations");
    if in_tree.is_dir() { in_tree } else { PathBuf::from("alfred-animations") }
}

impl Args {
    pub fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
        let mut a = Args {
            mode: Mode::Gpu,
            frames_dir: default_frames_dir(),
            poses: vec!["idle".into(), "listening".into()],
            fps: 16.0,
            pose_every: 8.0,
            char_scale: 1.0,
            textures: Textures::Gl,
            seconds: 30.0,
            fb: "/dev/fb0".into(),
            fb_geometry: None,
            screenshot: None,
            stats: None,
            no_vt: false,
            verbose: false,
            reset_console: false,
        };
        let mut mode_given = false;
        while let Some(flag) = it.next() {
            let mut val = || it.next().ok_or_else(|| format!("{flag}: missing value"));
            match flag.as_str() {
                "--mode" => {
                    mode_given = true;
                    a.mode = match val()?.as_str() {
                        "gpu" => Mode::Gpu,
                        "gpu-window" => Mode::GpuWindow,
                        "soft-blit" => Mode::SoftBlit,
                        "soft-slint" => Mode::SoftSlint,
                        other => return Err(format!("--mode: unknown mode {other}")),
                    }
                }
                "--frames-dir" => a.frames_dir = val()?.into(),
                "--poses" => {
                    let v = val()?;
                    a.poses = if v == "all" {
                        Vec::new()
                    } else {
                        v.split(',').filter(|s| !s.is_empty()).map(String::from).collect()
                    };
                }
                "--fps" => a.fps = num(&flag, &val()?)?,
                "--pose-every" => a.pose_every = num(&flag, &val()?)?,
                "--char-scale" => a.char_scale = num(&flag, &val()?)?,
                "--textures" => {
                    a.textures = match val()?.as_str() {
                        "gl" => Textures::Gl,
                        "upload" => Textures::Upload,
                        other => return Err(format!("--textures: expected gl or upload, got {other}")),
                    }
                }
                "--seconds" => a.seconds = num(&flag, &val()?)?,
                "--fb" => a.fb = val()?.into(),
                "--fb-geometry" => a.fb_geometry = Some(parse_geometry(&val()?)?),
                "--screenshot" => a.screenshot = Some(val()?.into()),
                "--stats" => a.stats = Some(val()?.into()),
                "--no-vt" => a.no_vt = true,
                "--verbose" => a.verbose = true,
                "--reset-console" => a.reset_console = true,
                "-h" | "--help" => return Err(String::new()),
                other => return Err(format!("unknown option {other}")),
            }
        }
        if !mode_given && !a.reset_console {
            return Err("--mode is required".into());
        }
        if !(a.fps > 0.0 && a.fps <= 120.0) {
            return Err(format!("--fps must be in (0, 120], got {}", a.fps));
        }
        if !(a.seconds > 0.0) {
            return Err(format!("--seconds must be > 0, got {}", a.seconds));
        }
        if !(0.25..=crate::layout::MAX_CHAR_SCALE).contains(&a.char_scale) {
            return Err(format!(
                "--char-scale must be in [0.25, {}] (the character must fit above the clock), got {}",
                crate::layout::MAX_CHAR_SCALE,
                a.char_scale
            ));
        }
        if a.pose_every < 0.0 {
            return Err("--pose-every must be >= 0".into());
        }
        Ok(a)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> Result<Args, String> {
        Args::parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn flags() {
        let a = parse("--mode soft-blit --poses idle,error --seconds 5 --fb-geometry 800x1280x3200 --char-scale 1.2")
            .unwrap();
        assert_eq!(a.mode, Mode::SoftBlit);
        assert_eq!(a.poses, vec!["idle", "error"]);
        assert_eq!(a.seconds, 5.0);
        assert_eq!(a.char_scale, 1.2);
        assert_eq!(a.fb_geometry, Some(Geometry { xres: 800, yres: 1280, line_length: 3200 }));
        assert!(parse("--mode gpu --poses all").unwrap().poses.is_empty());
        assert_eq!(parse("--mode gpu --textures upload").unwrap().textures, Textures::Upload);
        assert!(parse("--poses idle").is_err(), "mode is required");
        assert!(parse("--reset-console").is_ok());
        assert!(parse("--mode gpu --char-scale 2").is_err());
        assert!(parse("--mode gpu --seconds 0").is_err());
        assert!(parse("--mode nope").is_err());
        assert!(parse("--mode gpu --bogus").is_err());
    }
}
