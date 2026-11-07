// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The baselines without GPU, both on our own fbdev platform (Slint's
//! software renderer into a RAM back buffer, dirty rows copied to /dev/fb0):
//! - `soft-blit`: every frame is decoded, scaled to its on-screen size and
//!   converted to BGRX once at load; each tick copies one frame straight into
//!   the framebuffer at the character's box. Slint draws only the background
//!   and the clock (the character is not in its scene).
//! - `soft-slint`: the frames (pre-scaled RGB8 Slint images) are drawn by
//!   Slint's software renderer, as the M0 pose test did.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use serde_json::json;
use slint::{ComponentHandle, Image, SharedPixelBuffer};

use crate::app_sw::AppWindow;
use crate::args::{Args, Mode};
use crate::frames::{self, Animation};
use crate::layout;
use crate::pixel::Bgrx;
use crate::platform_fb::{self, Framebuffer, Output, VtGuard};
use crate::scene::{self, Cursor, Hooks, Timeline};
use crate::stats::Stats;

pub fn run(args: &Args, start: Instant, stats: &Rc<RefCell<Stats>>, anim: Animation) -> Result<(), String> {
    let fb = Framebuffer::open(&args.fb, args.fb_geometry)?;
    eprintln!("spike-gpu: {}: {}", args.fb.display(), fb.info);
    stats.borrow_mut().set_meta("fb_info", json!(fb.info));
    let g = fb.geometry;
    if g.xres < layout::SCREEN_W || g.yres < layout::SCREEN_H {
        return Err(format!(
            "framebuffer is {}x{}, the layout needs {}x{} portrait",
            g.xres,
            g.yres,
            layout::SCREEN_W,
            layout::SCREEN_H
        ));
    }

    let b = layout::char_box(anim.width, anim.height, args.char_scale);
    stats.borrow_mut().set_meta("char_box", json!([b.x, b.y, b.w, b.h]));

    // Prepare every frame once: decode, scale to the box, convert.
    let t0 = Instant::now();
    let mut blits: Vec<Vec<Rc<Vec<Bgrx>>>> = Vec::new();
    let mut images: Vec<Vec<Image>> = Vec::new();
    for pose in &anim.poses {
        let (mut bv, mut iv) = (Vec::new(), Vec::new());
        for png in &pose.pngs {
            let rgb = frames::scale_rgb(frames::decode_rgb(png)?, b.w, b.h);
            match args.mode {
                Mode::SoftBlit => bv.push(Rc::new(frames::rgb_to_bgrx(&rgb))),
                _ => iv.push(Image::from_rgb8(SharedPixelBuffer::clone_from_slice(&rgb.data, rgb.w, rgb.h))),
            }
        }
        blits.push(bv);
        images.push(iv);
    }
    let bpp = if args.mode == Mode::SoftBlit { 4 } else { 3 };
    {
        let mut s = stats.borrow_mut();
        s.set_meta("prepare_ms", json!(t0.elapsed().as_secs_f64() * 1e3));
        s.set_meta("frames_ram_mb", json!((anim.frame_count() * (b.w * b.h) as usize * bpp) as f64 / 1048576.0));
    }
    let counts: Vec<usize> = anim.poses.iter().map(|p| p.pngs.len()).collect();
    drop(anim);

    let vt = if !args.no_vt {
        VtGuard::enter().map_err(|e| eprintln!("spike-gpu: warning: console left in text mode: {e}")).ok()
    } else {
        None
    };
    stats.borrow_mut().set_meta("vt_graphics", json!(vt.is_some()));

    let window = platform_fb::install(start)?;
    let app = AppWindow::new().map_err(|e| format!("cannot create the UI: {e}"))?;
    let mut out = Output::new(fb);
    platform_fb::set_size(&window, &out);
    app.set_char_x(b.x as f32);
    app.set_char_y(b.y as f32);
    app.set_char_w(b.w as f32);
    app.set_char_h(b.h as f32);
    app.set_clock_y(layout::CLOCK_Y as f32);
    app.set_show_character(args.mode == Mode::SoftSlint);
    app.show().map_err(|e| format!("cannot show the UI: {e}"))?;

    let pending: Rc<Cell<Option<Cursor>>> = Rc::default();
    let show: Box<dyn Fn(Cursor)> = match args.mode {
        Mode::SoftBlit => {
            let pending = pending.clone();
            Box::new(move |c| pending.set(Some(c)))
        }
        _ => {
            let weak = app.as_weak();
            Box::new(move |c: Cursor| {
                if let Some(app) = weak.upgrade() {
                    app.set_character(images[c.pose][c.frame].clone());
                }
            })
        }
    };
    let weak = app.as_weak();
    let set_clock = Box::new(move |s: String| {
        if let Some(app) = weak.upgrade() {
            app.set_clock(s.into());
        }
    });
    let scene = scene::start(
        Timeline::new(args.fps, args.pose_every, counts),
        args.seconds,
        stats.clone(),
        Hooks { show, set_clock, on_quit: Box::new(|| {}) },
    );
    platform_fb::run(&window, &mut out, &scene, args.fps, || {
        pending.take().map(|c| (b, blits[c.pose][c.frame].clone()))
    })?;

    if let Some(path) = &args.screenshot {
        out.screenshot(path)?;
        eprintln!("spike-gpu: screenshot {}", path.display());
    }
    drop(vt);
    Ok(())
}
