// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Command line (hand-rolled: one less dependency on the device).

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    Winit,
    LinuxKms,
    Fbdev,
    Headless,
}

impl Backend {
    pub fn name(self) -> &'static str {
        match self {
            Backend::Winit => "winit",
            Backend::LinuxKms => "linuxkms",
            Backend::Fbdev => "fbdev",
            Backend::Headless => "headless",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderInto {
    /// Render straight into the mmap'ed framebuffer.
    Fb,
    /// Render into a RAM back buffer, then copy the dirty rows.
    Ram,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Geometry {
    pub xres: u32,
    pub yres: u32,
    pub line_length: u32,
}

#[derive(Clone, Debug)]
pub struct Args {
    pub backend: Backend,
    pub fb: PathBuf,
    pub fb_geometry: Option<Geometry>,
    pub fps: f64,
    pub pose_swap_every: f64,
    pub volume_step_every: f64,
    pub seconds: Option<f64>,
    pub frames: Option<usize>,
    pub screenshot: Option<PathBuf>,
    pub stats: Option<PathBuf>,
    pub render_into: RenderInto,
    pub no_vt: bool,
    pub verbose: bool,
    pub slint_animate: bool,
    pub no_box: bool,
    pub no_status: bool,
    pub reset_console: bool,
    pub full_swap: bool,
    pub poses: Vec<PathBuf>,
}

pub const USAGE: &str = "\
usage: spike-ui [options]
  --backend winit|linuxkms|fbdev|headless   (default: winit on x86_64, fbdev on aarch64)
  --fb PATH                 framebuffer device or plain file (default /dev/fb0)
  --fb-geometry WxHxLINE    geometry to use when the FBIOGET ioctls fail (plain file), e.g. 800x1280x3200
  --render-into fb|ram      fbdev: render into the mmap'ed framebuffer, or into RAM and copy dirty rows (default fb)
  --fps N                   scene tick rate (default 16)
  --pose-swap-every S       swap the pose every S seconds, 0 = never (default 5)
  --full-swap               the pose swap also changes the whole background (full-screen redraw)
  --volume-step-every S     step the volume bar every S seconds, 0 = never (default 2)
  --no-box                  keep the box still (isolates clock/volume/pose frames)
  --no-status               keep the status line constant (isolates the clock frame: run D)
  --slint-animate           move the box with Slint's `animate` (waypoints every 0.5 s) instead of per tick
  --pose A.png[,B.png]      poses to alternate (default: the two embedded placeholders)
  --seconds N | --frames N  exit after N seconds / N frames (fbdev, headless) or N ticks (winit, linuxkms)
  --screenshot FILE.png     screenshot at exit (key S in the simulator saves one too)
  --stats FILE.json         write the statistics as JSON at exit
  --no-vt                   do not switch the text console to graphics mode (fbdev)
  --verbose                 one line per frame on stderr
  --reset-console           only set /dev/tty0 back to text mode and exit (after a run left it in graphics mode)
";

fn default_backend() -> Backend {
    if cfg!(target_arch = "aarch64") { Backend::Fbdev } else { Backend::Winit }
}

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

impl Args {
    pub fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
        let mut a = Args {
            backend: default_backend(),
            fb: "/dev/fb0".into(),
            fb_geometry: None,
            fps: 16.0,
            pose_swap_every: 5.0,
            volume_step_every: 2.0,
            seconds: None,
            frames: None,
            screenshot: None,
            stats: None,
            render_into: RenderInto::Fb,
            no_vt: false,
            verbose: false,
            slint_animate: false,
            no_box: false,
            no_status: false,
            reset_console: false,
            full_swap: false,
            poses: Vec::new(),
        };
        while let Some(flag) = it.next() {
            let mut val = || it.next().ok_or_else(|| format!("{flag}: missing value"));
            match flag.as_str() {
                "--backend" => {
                    a.backend = match val()?.as_str() {
                        "winit" => Backend::Winit,
                        "linuxkms" => Backend::LinuxKms,
                        "fbdev" => Backend::Fbdev,
                        "headless" => Backend::Headless,
                        other => return Err(format!("--backend: unknown backend {other}")),
                    }
                }
                "--fb" => a.fb = val()?.into(),
                "--fb-geometry" => a.fb_geometry = Some(parse_geometry(&val()?)?),
                "--render-into" => {
                    a.render_into = match val()?.as_str() {
                        "fb" => RenderInto::Fb,
                        "ram" => RenderInto::Ram,
                        other => return Err(format!("--render-into: expected fb or ram, got {other}")),
                    }
                }
                "--fps" => a.fps = num(&flag, &val()?)?,
                "--pose-swap-every" => a.pose_swap_every = num(&flag, &val()?)?,
                "--volume-step-every" => a.volume_step_every = num(&flag, &val()?)?,
                "--seconds" => a.seconds = Some(num(&flag, &val()?)?),
                "--frames" => a.frames = Some(num(&flag, &val()?)?),
                "--screenshot" => a.screenshot = Some(val()?.into()),
                "--stats" => a.stats = Some(val()?.into()),
                "--pose" => a.poses = val()?.split(',').map(PathBuf::from).collect(),
                "--no-vt" => a.no_vt = true,
                "--verbose" => a.verbose = true,
                "--slint-animate" => a.slint_animate = true,
                "--no-box" => a.no_box = true,
                "--no-status" => a.no_status = true,
                "--reset-console" => a.reset_console = true,
                "--full-swap" => a.full_swap = true,
                "-h" | "--help" => return Err(String::new()),
                other => return Err(format!("unknown option {other}")),
            }
        }
        if !(a.fps > 0.0 && a.fps <= 120.0) {
            return Err(format!("--fps must be in (0, 120], got {}", a.fps));
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
        let a = parse("--backend headless --frames 48 --render-into ram --fb-geometry 800x1280x3200").unwrap();
        assert_eq!(a.backend, Backend::Headless);
        assert_eq!(a.frames, Some(48));
        assert_eq!(a.render_into, RenderInto::Ram);
        assert_eq!(a.fb_geometry, Some(Geometry { xres: 800, yres: 1280, line_length: 3200 }));
        assert!(parse("--fb-geometry 800x1280x100").is_err());
        assert!(parse("--bogus").is_err());
        assert!(parse("--fps").is_err());
    }
}
