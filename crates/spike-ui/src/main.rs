// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! M0 UI spike: Slint's software renderer on the ThinkSmart View panel.
//! See README.md for the backends, the measurements and the device runbook.

mod args;
mod measure;
mod pixel;
mod platform_fb;
mod scene;
mod shot;
mod stats;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

use serde_json::json;
use slint::{ComponentHandle, Image};

use args::{Args, Backend, RenderInto, USAGE};
use scene::SceneConfig;
use stats::Stats;

slint::include_modules!();

static POSE_A: &[u8] = include_bytes!("../fixtures/pose-a.png");
static POSE_B: &[u8] = include_bytes!("../fixtures/pose-b.png");

fn main() {
    let start = Instant::now();
    let args = match Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(e) if e.is_empty() => {
            print!("{USAGE}");
            return;
        }
        Err(e) => {
            eprintln!("spike-ui: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    if args.reset_console {
        match platform_fb::reset_console() {
            Ok(()) => eprintln!("spike-ui: /dev/tty0 set back to text mode"),
            Err(e) => {
                eprintln!("spike-ui: error: {e}");
                std::process::exit(1);
            }
        }
        return;
    }
    if let Err(e) = run(&args, start) {
        eprintln!("spike-ui: error: {e}");
        std::process::exit(1);
    }
}

fn load_poses(args: &Args) -> Result<Vec<Image>, String> {
    if args.poses.is_empty() {
        return Ok(vec![shot::pose_from_bytes(POSE_A), shot::pose_from_bytes(POSE_B)]);
    }
    args.poses.iter().map(|p| shot::load_pose(p)).collect()
}

fn run(args: &Args, start: Instant) -> Result<(), String> {
    let stats = Rc::new(RefCell::new(Stats::new(start, args.fps, args.verbose)));
    {
        let mut s = stats.borrow_mut();
        s.set_meta("backend", json!(args.backend.name()));
        s.set_meta("target_arch", json!(std::env::consts::ARCH));
        s.set_meta("slint_animate", json!(args.slint_animate));
        s.set_meta("full_swap", json!(args.full_swap));
        s.set_meta("no_box", json!(args.no_box));
        s.set_meta("no_status", json!(args.no_status));
    }
    // Installed before any backend starts, so a Ctrl-C never kills the process
    // with the console in graphics mode (ours or Slint's linuxfb path).
    platform_fb::install_signal_handlers();
    match args.backend {
        Backend::Fbdev | Backend::Headless => run_fb(args, start, &stats)?,
        Backend::Winit => run_slint_backend(args, "winit", &stats)?,
        Backend::LinuxKms => run_slint_backend(args, "linuxkms", &stats)?,
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
    }
    Ok(())
}

/// Our own platform: fbdev (device or plain file) or headless (RAM only).
fn run_fb(args: &Args, start: Instant, stats: &Rc<RefCell<Stats>>) -> Result<(), String> {
    let fb = if args.backend == Backend::Fbdev {
        let fb = platform_fb::Framebuffer::open(&args.fb, args.fb_geometry)?;
        eprintln!("spike-ui: {}: {}", args.fb.display(), fb.info);
        stats.borrow_mut().set_meta("fb_info", json!(fb.info));
        Some(fb)
    } else {
        None
    };
    let vt = if args.backend == Backend::Fbdev && !args.no_vt {
        match platform_fb::VtGuard::enter() {
            Ok(g) => Some(g),
            Err(e) => {
                eprintln!("spike-ui: warning: console left in text mode: {e}");
                None
            }
        }
    } else {
        None
    };
    stats.borrow_mut().set_meta("vt_graphics", json!(vt.is_some()));

    let window = platform_fb::install(start)?;
    let app = AppWindow::new().map_err(|e| format!("cannot create the UI: {e}"))?;
    let mut out = platform_fb::Output::new(fb, args.render_into);
    stats.borrow_mut().set_meta(
        "render_into",
        json!(match out.mode {
            RenderInto::Fb => "fb",
            RenderInto::Ram => "ram",
        }),
    );
    platform_fb::set_size(&window, &out);
    app.show().map_err(|e| format!("cannot show the UI: {e}"))?;

    let poses = load_poses(args)?;
    let cfg = SceneConfig { count_ticks: false, on_quit: Box::new(|| {}) };
    let scene = scene::start(&app, args, poses, stats.clone(), cfg);
    platform_fb::run(&window, &mut out, &scene, args)?;

    if let Some(path) = &args.screenshot {
        out.screenshot(path)?;
        eprintln!("spike-ui: screenshot {}", path.display());
    }
    drop(vt);
    Ok(())
}

/// Slint's own backends (winit window, linuxkms); Slint renders internally,
/// so only ticks, RSS and CPU are measured.
#[cfg(any(feature = "simulator", feature = "linuxkms"))]
fn run_slint_backend(args: &Args, name: &str, stats: &Rc<RefCell<Stats>>) -> Result<(), String> {
    let compiled = match name {
        "winit" => cfg!(feature = "simulator"),
        _ => cfg!(feature = "linuxkms"),
    };
    if !compiled {
        return Err(format!("this binary was built without the Slint backend '{name}'"));
    }
    slint::BackendSelector::new()
        .backend_name(name.into())
        .renderer_name("software".into())
        .select()
        .map_err(|e| format!("Slint backend '{name}' (software renderer) refused to start: {e}"))?;
    let app = AppWindow::new().map_err(|e| format!("Slint backend '{name}' could not create the window: {e}"))?;

    let failure: Rc<RefCell<Option<String>>> = Rc::default();
    let snapshot = |app: &AppWindow, path: &std::path::Path| -> Result<(), String> {
        let buf = app.window().take_snapshot().map_err(|e| format!("take_snapshot: {e}"))?;
        shot::write_rgba(path, &buf)?;
        eprintln!("spike-ui: screenshot {}", path.display());
        Ok(())
    };

    // Key S: numbered screenshots next to --screenshot (or in out/).
    let key_shots = Rc::new(std::cell::Cell::new(0u32));
    let base = args.screenshot.clone().unwrap_or_else(|| "out/spike-ui.png".into());
    let weak = app.as_weak();
    app.on_screenshot_requested(move || {
        let Some(app) = weak.upgrade() else { return };
        key_shots.set(key_shots.get() + 1);
        let stem = base.file_stem().and_then(|s| s.to_str()).unwrap_or("spike-ui");
        let path = base.with_file_name(format!("{stem}-key{}.png", key_shots.get()));
        if let Err(e) = snapshot(&app, &path) {
            eprintln!("spike-ui: {e}");
        }
    });

    app.on_box_touched(|| eprintln!("spike-ui: touch on the box"));

    let weak = app.as_weak();
    let shot_path = args.screenshot.clone();
    let failure2 = failure.clone();
    let on_quit = Box::new(move || {
        if let (Some(app), Some(path)) = (weak.upgrade(), shot_path.as_ref()) {
            if let Err(e) = snapshot(&app, path) {
                *failure2.borrow_mut() = Some(e);
            }
        }
        let _ = slint::quit_event_loop();
    });
    let scene = scene::start(&app, args, load_poses(args)?, stats.clone(), SceneConfig { count_ticks: true, on_quit });
    app.run().map_err(|e| format!("Slint backend '{name}' failed: {e}"))?;
    drop(scene);
    match failure.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

#[cfg(not(any(feature = "simulator", feature = "linuxkms")))]
fn run_slint_backend(_: &Args, name: &str, _: &Rc<RefCell<Stats>>) -> Result<(), String> {
    Err(format!("this binary was built without the Slint backend '{name}' (features: simulator, linuxkms)"))
}
