// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! GPU spike: can the ThinkSmart View animate the character at 16 fps
//! cheaply? The same scene (Alfred's animation frames + a large clock) in
//! three ways: Slint femtovg on KMS (GPU), pre-scaled frames copied into
//! /dev/fb0 (soft-blit), Slint's software renderer (soft-slint).
//! See README.md for the device commands and the numbers to record.

mod args;
mod frames;
#[cfg(any(feature = "gpu", feature = "gpu-window"))]
mod gpu;
mod layout;
mod measure;
mod pixel;
mod platform_fb;
mod scene;
mod shot;
mod soft;
mod stats;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use serde_json::json;

use args::{Args, Mode, USAGE};
use stats::Stats;

/// The UI compiled for femtovg (font embedded as a file).
#[allow(dead_code)]
mod app_gpu {
    include!(concat!(env!("OUT_DIR"), "/app_gpu.rs"));
}

/// The same UI with glyphs pre-rendered for the software renderer.
mod app_sw {
    include!(concat!(env!("OUT_DIR"), "/app_sw.rs"));
}

fn main() {
    let start = Instant::now();
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) if e.is_empty() => {
            print!("{USAGE}");
            println!("modes in this binary: {}", compiled_modes().join(", "));
            return;
        }
        Err(e) => {
            eprintln!("spike-gpu: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if args.reset_console {
        match platform_fb::reset_console() {
            Ok(()) => eprintln!("spike-gpu: /dev/tty0 set back to text mode"),
            Err(e) => {
                eprintln!("spike-gpu: error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Err(e) = run(&args, start) {
        eprintln!("spike-gpu: error: {e}");
        std::process::exit(1);
    }
}

fn compiled_modes() -> Vec<&'static str> {
    let mut m = vec!["soft-blit", "soft-slint"];
    if cfg!(feature = "gpu") {
        m.push("gpu");
    }
    if cfg!(feature = "gpu-window") {
        m.push("gpu-window");
    }
    m
}

fn run(args: &Args, start: Instant) -> Result<(), String> {
    let stats = Rc::new(RefCell::new(Stats::new(start, args.fps, args.verbose)));
    // Installed before anything touches the screen: a Ctrl-C/SIGTERM ends the
    // run through the normal path, so the console and the CRTC are restored.
    platform_fb::install_signal_handlers();

    let anim = frames::load(&args.frames_dir, &args.poses)?;
    eprintln!(
        "spike-gpu: {} poses ({}), {} frames of {}x{} from {} ({:.1} MB of PNG, read in {:.0} ms)",
        anim.poses.len(),
        anim.poses.iter().map(|p| format!("{} {}", p.name, p.pngs.len())).collect::<Vec<_>>().join(", "),
        anim.frame_count(),
        anim.width,
        anim.height,
        anim.dir.display(),
        anim.png_bytes as f64 / 1048576.0,
        anim.read_ms
    );
    {
        let mut s = stats.borrow_mut();
        s.set_meta("mode", json!(args.mode.name()));
        s.set_meta("target_arch", json!(std::env::consts::ARCH));
        if args.mode.is_gpu() {
            s.set_meta("textures", json!(format!("{:?}", args.textures).to_lowercase()));
        }
        s.set_meta("char_scale", json!(args.char_scale));
        s.set_meta("pose_every_s", json!(args.pose_every));
        s.set_meta("seconds", json!(args.seconds));
        s.set_meta(
            "poses",
            json!(anim.poses.iter().map(|p| json!({"name": p.name, "frames": p.pngs.len()})).collect::<Vec<_>>()),
        );
        s.set_meta("frame_size", json!([anim.width, anim.height]));
        s.set_meta("png_read_ms", json!(anim.read_ms));
        s.set_meta("png_mb", json!(anim.png_bytes as f64 / 1048576.0));
    }

    match args.mode {
        Mode::SoftBlit | Mode::SoftSlint => soft::run(args, start, &stats, anim)?,
        #[cfg(any(feature = "gpu", feature = "gpu-window"))]
        Mode::Gpu | Mode::GpuWindow => gpu::run(args, &stats, anim)?,
        #[cfg(not(any(feature = "gpu", feature = "gpu-window")))]
        Mode::Gpu | Mode::GpuWindow => {
            return Err(format!("this binary was built without the GPU modes (feature gpu): --mode {}", args.mode.name()));
        }
    }
    if platform_fb::stop_requested() {
        stats.borrow_mut().set_meta("stopped_by_signal", json!(true));
    }
    let s = stats.borrow();
    let summary = s.summary();
    s.print_table(&summary);
    if let Some(path) = &args.stats {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&summary).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
        eprintln!("spike-gpu: stats {}", path.display());
    }
    Ok(())
}
