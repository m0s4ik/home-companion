// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Renderer selection (GPU first, software fallback, never a black screen) and the UI loop.
//!
//! - `gpu`: Slint's linuxkms backend with femtovg (EGL/GBM, page flipping). A Slint platform
//!   can be installed only once per process, so when the GPU path fails `home-companion` re-executes
//!   itself with `--renderer software` (env `HOME_COMPANION_RENDERER_FALLBACK=1`); this happens before
//!   the audio and the Wyoming server are opened.
//! - `software`: our fbdev platform rendering into RAM and copying the dirty rows to /dev/fb0.
//! - `headless`: the same platform with no framebuffer (CI, screenshots).
//! - `window`: winit + the software renderer (the simulator).
//!
//! On every backend a 20 ms `slint::Timer` drains the core's commands and the touch events.

use std::cell::Cell;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

use slint::platform::software_renderer::MinimalSoftwareWindow;
use slint::{LogicalPosition, Timer, TimerMode};

use super::Screen;
use super::backlight::WakeSource;
use super::platform_fb::{self, Framebuffer, Output, VtGuard};
use super::screen::{Ui, UiOptions, save_png};
use super::touch::{TouchEvent, TouchKind};
use crate::core::{CoreEvent, UiCommand};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Gpu,
    Software,
    Window,
    Headless,
}

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Gpu => "gpu",
            Kind::Software => "software",
            Kind::Window => "window",
            Kind::Headless => "headless",
        }
    }
}

fn can_dlopen(name: &str) -> bool {
    let c = std::ffi::CString::new(name).unwrap();
    // SAFETY: dlopen/dlclose of a system library by name; nothing is called in it.
    unsafe {
        let h = libc::dlopen(c.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if h.is_null() {
            return false;
        }
        libc::dlclose(h);
    }
    true
}

/// Why the GPU path is not usable here (None = try it).
pub fn gpu_unavailable() -> Option<String> {
    if !cfg!(feature = "gpu") {
        return Some("built without the gpu feature".into());
    }
    if let Err(e) = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/dri/card0")
    {
        return Some(format!("/dev/dri/card0: {e}"));
    }
    for lib in ["libgbm.so.1", "libEGL.so.1"] {
        if !can_dlopen(lib) {
            return Some(format!(
                "{lib} cannot be loaded (LD_LIBRARY_PATH=/opt/mesa-freedreno/lib on the device)"
            ));
        }
    }
    None
}

/// `ui.renderer` → what runs. `auto`: GPU if usable, else the framebuffer, else a window.
pub fn choose(name: &str) -> Kind {
    match name {
        "gpu" => Kind::Gpu,
        "software" => Kind::Software,
        "window" => Kind::Window,
        "headless" => Kind::Headless,
        _ => match gpu_unavailable() {
            None => Kind::Gpu,
            Some(why) => {
                log::info!("renderer auto: no GPU ({why})");
                if cfg!(feature = "fbdev") && Path::new("/dev/fb0").exists() {
                    Kind::Software
                } else if cfg!(feature = "simulator")
                    && std::env::var_os("DISPLAY")
                        .or(std::env::var_os("WAYLAND_DISPLAY"))
                        .is_some()
                {
                    Kind::Window
                } else {
                    Kind::Headless
                }
            }
        },
    }
}

/// The panel's native size (width, height) from sysfs, for `rotation = auto`.
fn panel_size() -> Option<(u32, u32)> {
    if let Ok(dir) = std::fs::read_dir("/sys/class/drm") {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.starts_with("card0-") {
                continue;
            }
            if let Ok(modes) = std::fs::read_to_string(e.path().join("modes")) {
                if let Some((w, h)) = modes.lines().next().and_then(|l| l.split_once('x')) {
                    if let (Ok(w), Ok(h)) = (
                        w.trim().parse(),
                        h.trim()
                            .trim_end_matches(|c: char| !c.is_ascii_digit())
                            .parse(),
                    ) {
                        return Some((w, h));
                    }
                }
            }
        }
    }
    let v = std::fs::read_to_string("/sys/class/graphics/fb0/virtual_size").ok()?;
    let (w, h) = v.trim().split_once(',')?;
    Some((w.parse().ok()?, h.parse().ok()?))
}

/// `ui.rotation` in degrees; `auto` = 270 on a portrait panel (the ThinkSmart View standing in
/// landscape, as on the device), else 0.
pub fn rotation(setting: &str, kind: Kind) -> u32 {
    match setting {
        "90" => 90,
        "180" => 180,
        "270" => 270,
        "0" => 0,
        _ => match kind {
            Kind::Gpu | Kind::Software => match panel_size() {
                Some((w, h)) if h > w => 270,
                _ => 0,
            },
            _ => 0,
        },
    }
}

/// Re-executes home-companion with the software renderer (the GPU path failed before anything else was
/// opened). Returns only if exec itself failed.
pub fn reexec_software(why: &str) -> String {
    log::warn!(event = "renderer_fallback", reason = why; "GPU renderer failed ({why}): restarting with the software renderer");
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => return format!("current_exe: {e}"),
    };
    let mut args: Vec<std::ffi::OsString> = Vec::new();
    let mut it = std::env::args_os().skip(1);
    while let Some(a) = it.next() {
        if a == "--renderer" {
            it.next();
            continue;
        }
        args.push(a);
    }
    args.push("--renderer".into());
    args.push("software".into());
    let err = std::process::Command::new(exe)
        .args(args)
        .env("HOME_COMPANION_RENDERER_FALLBACK", "1")
        .exec();
    format!("exec: {err}")
}

/// The window and whatever must live as long as it (console guard, fbdev loop).
pub struct Prepared<S: Screen> {
    pub screen: S,
    pub kind: Kind,
    pub rotation: u32,
    fb: Option<(Rc<MinimalSoftwareWindow>, Output)>,
    _vt: Option<VtGuard>,
}

/// Software, headless: our platform, the `sw` build of the screen.
pub fn prepare_fb(
    kind: Kind,
    framebuffer: &Path,
    rotation: u32,
) -> Result<Prepared<super::sw::CompanionWindow>, String> {
    let fb = if kind == Kind::Software {
        match Framebuffer::open(framebuffer) {
            Ok(f) => {
                log::info!("framebuffer: {}", f.info);
                Some(f)
            }
            Err(e) => {
                // Nothing to show on, but the satellite still works.
                log::error!("{e}: running headless");
                None
            }
        }
    } else {
        None
    };
    let rotation = if fb.is_some() { rotation } else { 0 };
    let vt = if fb.is_some() {
        VtGuard::enter()
            .map_err(|e| log::warn!("console left in text mode: {e}"))
            .ok()
    } else {
        None
    };
    let window = platform_fb::install()?;
    let out = Output::new(fb, rotation);
    window.set_size(out.window_size());
    let screen =
        <super::sw::CompanionWindow as Screen>::create().map_err(|e| format!("window: {e}"))?;
    Ok(Prepared {
        screen,
        kind,
        rotation,
        fb: Some((window, out)),
        _vt: vt,
    })
}

/// The simulator's window (winit, software renderer).
pub fn prepare_window() -> Result<Prepared<super::sw::CompanionWindow>, String> {
    slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .select()
        .map_err(|e| format!("winit backend (software renderer): {e}"))?;
    let screen =
        <super::sw::CompanionWindow as Screen>::create().map_err(|e| format!("window: {e}"))?;
    Ok(Prepared {
        screen,
        kind: Kind::Window,
        rotation: 0,
        fb: None,
        _vt: None,
    })
}

/// The GPU path. Err = fall back (re-exec) with the reason.
#[cfg(feature = "gpu")]
pub fn prepare_gpu(rotation: u32) -> Result<Prepared<super::gpu::CompanionWindow>, String> {
    if let Some(why) = gpu_unavailable() {
        return Err(why);
    }
    let vt = VtGuard::enter()
        .map_err(|e| log::warn!("console left in text mode: {e}"))
        .ok();
    slint::BackendSelector::new()
        .backend_name("linuxkms".into())
        .renderer_name("femtovg".into())
        .select()
        .map_err(|e| format!("linuxkms + femtovg: {e}"))?;
    let screen = <super::gpu::CompanionWindow as Screen>::create()
        .map_err(|e| format!("linuxkms + femtovg window: {e}"))?;
    let size = screen.window().size();
    log::info!(
        "gpu output {}x{} (rotation {rotation})",
        size.width,
        size.height
    );
    Ok(Prepared {
        screen,
        kind: Kind::Gpu,
        rotation,
        fb: None,
        _vt: vt,
    })
}

/// What the UI loop needs besides the screen.
pub struct UiContext {
    pub character: Rc<character::Character>,
    pub opts: UiOptions,
    pub core: Sender<CoreEvent>,
    pub commands: Receiver<UiCommand>,
    pub touch: Option<Receiver<TouchEvent>>,
    /// Volume keys and the accelerometer (sensors.rs).
    pub wake: Option<Receiver<WakeSource>>,
    /// The weather thread (crate::ha): `None` = no real data.
    pub weather: Option<Receiver<Option<crate::ha::weather::Weather>>>,
    /// The shopping list thread (crate::ha::todo): its open items, and the ticks back.
    pub shopping: Option<(
        Receiver<Vec<crate::ha::todo::Item>>,
        std::sync::mpsc::Sender<crate::ha::todo::ShopCmd>,
    )>,
    /// The setup of Home Assistant (crate::ha::setup): Settings → Home Assistant.
    pub ha_setup: Option<crate::ha::setup::Setup>,
    pub fps: u32,
    pub exit_after: Option<f64>,
    pub screenshot: Option<PathBuf>,
    pub stop_requested: fn() -> bool,
    /// "Casa", Home Assistant's frontend (`[web]`, None = off).
    pub web: Option<super::webview::WebOptions>,
}

/// Runs the screen until quit (signal, `--exit-after`, window closed).
pub fn run<S: Screen>(p: Prepared<S>, ctx: UiContext) -> Result<(), String> {
    let Prepared {
        screen,
        kind,
        fb,
        _vt,
        ..
    } = p;
    let ui = Ui::new(screen, ctx.character.clone(), ctx.opts, ctx.core.clone())?;
    // The Home Assistant button opens "Casa" instead of telling the core.
    let casa = ctx.web.map(|mut w| {
        w.gpu = kind == Kind::Gpu;
        let casa = super::webview::Casa::new(ui.clone(), w);
        let c = Rc::downgrade(&casa);
        let u = Rc::downgrade(&ui);
        ui.screen.on_home_assistant_clicked(move || {
            if u.upgrade()
                .is_some_and(|u| u.link_instead_of_home_assistant())
            {
                return;
            }
            if let Some(c) = c.upgrade() {
                c.open();
            }
        });
        casa
    });
    let quit = Rc::new(Cell::new(false));
    let start = Instant::now();

    let drain = Timer::default();
    {
        let (ui, quit, casa) = (ui.clone(), quit.clone(), casa.clone());
        let (commands, touch, wake) = (ctx.commands, ctx.touch, ctx.wake);
        let weather = ctx.weather;
        let shopping = ctx.shopping.map(|(rx, tx)| {
            ui.set_shopping_sender(tx);
            rx
        });
        if let Some(s) = ctx.ha_setup {
            ui.set_ha_setup(s);
        }
        let (exit_after, screenshot, stop) =
            (ctx.exit_after, ctx.screenshot.clone(), ctx.stop_requested);
        let pressed = Cell::new(false);
        drain.start(TimerMode::Repeated, Duration::from_millis(20), move || {
            while let Ok(cmd) = commands.try_recv() {
                ui.apply(cmd);
            }
            ui.poll_mirror();
            if let Some(t) = &touch {
                while let Ok(ev) = t.try_recv() {
                    if ev.kind == TouchKind::Down {
                        ui.wake(WakeSource::Touch);
                    }
                    if casa.as_ref().is_some_and(|c| c.touch(&ev)) {
                        continue;
                    }
                    let position = LogicalPosition::new(ev.x, ev.y);
                    let button = slint::platform::PointerEventButton::Left;
                    let e = match ev.kind {
                        TouchKind::Down => {
                            pressed.set(true);
                            ui.screen.window().dispatch_event(
                                slint::platform::WindowEvent::PointerMoved { position },
                            );
                            slint::platform::WindowEvent::PointerPressed { position, button }
                        }
                        TouchKind::Move => slint::platform::WindowEvent::PointerMoved { position },
                        TouchKind::Up if pressed.replace(false) => {
                            slint::platform::WindowEvent::PointerReleased { position, button }
                        }
                        TouchKind::Up => continue,
                    };
                    ui.screen.window().dispatch_event(e);
                }
            }
            if let Some(w) = &wake {
                while let Ok(why) = w.try_recv() {
                    ui.wake(why);
                }
            }
            // After the wake events: the pair's own key presses never wake what it put to sleep.
            if super::backlight::take_sleep_request() {
                ui.sleep();
            }
            if let Some(w) = &weather {
                // Only the latest matters.
                if let Some(last) = w.try_iter().last() {
                    ui.set_weather(last);
                }
            }
            if let Some(s) = &shopping
                && let Some(items) = s.try_iter().last()
            {
                ui.set_shopping_items(items);
            }
            if crate::signals::take_wake() {
                log::info!("SIGUSR1: waking the screen");
                ui.wake(WakeSource::Signal);
            }
            if crate::signals::take_shot() {
                log::info!("SIGUSR2: a screenshot");
                ui.request_screenshot();
            }
            ui.poll();
            let expired = exit_after.is_some_and(|s| start.elapsed().as_secs_f64() >= s);
            if (expired || stop()) && !quit.get() {
                if let Some(path) = &screenshot {
                    if let Err(e) = save_png(&ui.screen, path) {
                        log::error!("screenshot {}: {e}", path.display());
                    }
                }
                quit.set(true);
                let _ = slint::quit_event_loop();
            }
        });
    }
    let clock = Timer::default();
    {
        let ui = ui.clone();
        let last = Cell::new(Instant::now());
        clock.start(TimerMode::Repeated, Duration::from_secs(1), move || {
            ui.tick_clock();
            if last.get().elapsed() >= Duration::from_secs(60) {
                last.set(Instant::now());
                let (frames, cpu) = ui.take_frame_stats();
                log::info!(
                    event = "screen_stats",
                    frames = frames,
                    frame_cpu_ms = if frames > 0 { cpu.as_secs_f64() * 1e3 / frames as f64 } else { 0.0 },
                    backlight = ui.backlight_level().map_or(-1, |l| l as i64);
                    "screen: {frames} animation frames in the last minute"
                );
            }
        });
    }
    // No rendering notifier on the GPU path: with one, Slint 1.18's femtovg renderer sizes the
    // canvas from the window instead of the rotated surface, and a rotated screen (270 on the
    // ThinkSmart View) is drawn shifted off the panel.

    log::info!(event = "renderer", renderer = kind.name(); "renderer {}", kind.name());
    let r = match fb {
        Some((window, mut out)) => {
            platform_fb::run(&window, &mut out, &quit, ctx.fps.max(15));
            Ok(())
        }
        None => ui.screen.run().map_err(|e| format!("event loop: {e}")),
    };
    ui.on_exit();
    r
}
