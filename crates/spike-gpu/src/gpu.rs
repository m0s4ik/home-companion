// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! GPU mode: Slint's linuxkms backend with the femtovg renderer (OpenGL ES
//! through EGL on a GBM surface; every frame is a full redraw followed by a
//! page flip on vsync). `gpu-window` runs the same code in a desktop window
//! (winit + femtovg) so it can be checked on the build machine.
//!
//! The frames reach the GPU in one of two ways (`--textures`):
//! - `gl`: at the first frame (Slint's `RenderingSetup` notification, GL
//!   context current) every PNG is decoded and uploaded once into its own GL
//!   texture, then handed to Slint as a borrowed texture; the CPU copy is
//!   dropped. Playing only switches the texture id: no uploads.
//! - `upload`: plain Slint images (RGB8 in RAM). femtovg's texture cache has no
//!   key for pixel buffers, so each change of the image uploads a new texture.
//!
//! Timing comes from Slint's rendering notifier: BeforeRendering ->
//! AfterRendering is the CPU time femtovg spends building and submitting the
//! GL commands; the frame interval and the process CPU per frame are taken at
//! AfterRendering. The swap and the wait for the previous page flip happen
//! right after AfterRendering, inside Slint.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Instant;

use serde_json::json;
use slint::{ComponentHandle, Image, RenderingState, SharedPixelBuffer};

use crate::app_gpu::AppWindow;
use crate::args::{Args, Mode, Textures};
use crate::frames::{self, Animation};
use crate::layout;
use crate::measure;
use crate::platform_fb::VtGuard;
use crate::scene::{self, Cursor, Hooks, Timeline};
use crate::shot;
use crate::stats::{Class, FrameRec, Stats};

type Frames = Rc<RefCell<Vec<Vec<Image>>>>;

pub fn run(args: &Args, stats: &Rc<RefCell<Stats>>, anim: Animation) -> Result<(), String> {
    let backend = match args.mode {
        Mode::Gpu if cfg!(feature = "gpu") => "linuxkms",
        Mode::GpuWindow if cfg!(feature = "gpu-window") => "winit",
        m => {
            return Err(format!(
                "this binary was built without mode '{}' (features: gpu for --mode gpu, gpu-window for --mode gpu-window)",
                m.name()
            ));
        }
    };
    // The text console: graphics mode, so fbcon neither blinks nor redraws
    // under us; restored when the guard drops (after the event loop).
    let vt = if args.mode == Mode::Gpu && !args.no_vt {
        VtGuard::enter().map_err(|e| eprintln!("spike-gpu: warning: console left in text mode: {e}")).ok()
    } else {
        None
    };
    stats.borrow_mut().set_meta("vt_graphics", json!(vt.is_some()));

    slint::BackendSelector::new()
        .backend_name(backend.into())
        .renderer_name("femtovg".into())
        .select()
        .map_err(|e| format!("Slint backend '{backend}' with the femtovg renderer refused to start: {e}"))?;
    let t_window = Instant::now();
    let app = AppWindow::new().map_err(|e| format!("Slint '{backend}' + femtovg could not create the window: {e}"))?;
    stats.borrow_mut().set_meta("window_create_ms", json!(t_window.elapsed().as_secs_f64() * 1e3));

    let size = app.window().size();
    if (size.width, size.height) != (layout::SCREEN_W, layout::SCREEN_H) {
        eprintln!(
            "spike-gpu: warning: the output is {}x{}, the layout is for {}x{} portrait",
            size.width,
            size.height,
            layout::SCREEN_W,
            layout::SCREEN_H
        );
    }
    stats.borrow_mut().set_meta("output_size", json!([size.width, size.height]));

    let b = layout::char_box(anim.width, anim.height, args.char_scale);
    app.set_char_x(b.x as f32);
    app.set_char_y(b.y as f32);
    app.set_char_w(b.w as f32);
    app.set_char_h(b.h as f32);
    app.set_clock_y(layout::CLOCK_Y as f32);
    app.set_show_character(true);
    stats.borrow_mut().set_meta("char_box", json!([b.x, b.y, b.w, b.h]));

    let frames: Frames = Rc::default();
    if args.textures == Textures::Upload {
        let t0 = Instant::now();
        let mut all = Vec::new();
        for pose in &anim.poses {
            let mut v = Vec::with_capacity(pose.pngs.len());
            for png in &pose.pngs {
                let rgb = frames::decode_rgb(png)?;
                v.push(Image::from_rgb8(SharedPixelBuffer::clone_from_slice(&rgb.data, rgb.w, rgb.h)));
            }
            all.push(v);
        }
        let mb = (anim.frame_count() * (anim.width * anim.height * 3) as usize) as f64 / 1048576.0;
        let mut s = stats.borrow_mut();
        s.set_meta("decode_ms", json!(t0.elapsed().as_secs_f64() * 1e3));
        s.set_meta("frames_ram_mb", json!(mb));
        *frames.borrow_mut() = all;
    }

    let cursor = Rc::new(Cell::new(Cursor::default()));
    let weak = app.as_weak();
    let (frames2, cursor2) = (frames.clone(), cursor.clone());
    let show = move |c: Cursor| {
        cursor2.set(c);
        let f = frames2.borrow();
        if let (Some(app), Some(img)) = (weak.upgrade(), f.get(c.pose).and_then(|p| p.get(c.frame))) {
            app.set_character(img.clone());
        }
    };
    let weak = app.as_weak();
    let set_clock = move |s: String| {
        if let Some(app) = weak.upgrade() {
            app.set_clock(s.into());
        }
    };
    let failure: Rc<RefCell<Option<String>>> = Rc::default();
    let (weak, failure2, shot_path) = (app.as_weak(), failure.clone(), args.screenshot.clone());
    let on_quit = move || {
        if let (Some(app), Some(path)) = (weak.upgrade(), shot_path.as_ref()) {
            let r = app
                .window()
                .take_snapshot()
                .map_err(|e| format!("take_snapshot: {e}"))
                .and_then(|buf| shot::write_rgba(path, &buf));
            match r {
                Ok(()) => eprintln!("spike-gpu: screenshot {}", path.display()),
                Err(e) => *failure2.borrow_mut() = Some(e),
            }
        }
        let _ = slint::quit_event_loop();
    };
    let show = Rc::new(show);
    let show2 = show.clone();
    let counts = anim.poses.iter().map(|p| p.pngs.len()).collect();
    let scene = scene::start(
        Timeline::new(args.fps, args.pose_every, counts),
        args.seconds,
        stats.clone(),
        Hooks { show: Box::new(move |c| show2(c)), set_clock: Box::new(set_clock), on_quit: Box::new(on_quit) },
    );

    // Rendering notifier: texture upload at setup, per-frame timing.
    let anim = RefCell::new(Some(anim));
    let (stats2, changes, quit) = (stats.clone(), scene.changes.clone(), scene.quit.clone());
    let textures = args.textures;
    let started = Cell::new(None::<(Instant, f64)>);
    let first = Cell::new(true);
    let setup_error = failure.clone();
    app.window()
        .set_rendering_notifier(move |state, api| match state {
            RenderingState::RenderingSetup => {
                let info = gl_info(api);
                let mut s = stats2.borrow_mut();
                s.set_meta("gl", info);
                if textures == Textures::Gl {
                    let Some(anim) = anim.borrow_mut().take() else { return };
                    match upload_textures(api, &anim) {
                        Ok((imgs, meta)) => {
                            s.set_meta("upload", meta);
                            *frames.borrow_mut() = imgs;
                            drop(s);
                            show(cursor.get());
                        }
                        Err(e) => {
                            *setup_error.borrow_mut() = Some(format!("texture upload failed: {e}"));
                            let _ = slint::quit_event_loop();
                        }
                    }
                }
            }
            RenderingState::BeforeRendering => {
                started.set(Some((Instant::now(), measure::thread_cpu_seconds())));
            }
            RenderingState::AfterRendering => {
                // Frames rendered for the exit screenshot are not counted.
                let Some((t0, cpu0)) = started.take().filter(|_| !quit.get()) else { return };
                let rec = FrameRec {
                    render_ms: Some(t0.elapsed().as_secs_f64() * 1e3),
                    thread_cpu_ms: Some((measure::thread_cpu_seconds() - cpu0) * 1e3),
                    ..Default::default()
                };
                let class = Class::from_changes(changes.replace(0), first.replace(false));
                stats2.borrow_mut().frame(class, rec);
            }
            _ => {}
        })
        .map_err(|e| format!("cannot set the rendering notifier: {e:?}"))?;

    app.run().map_err(|e| format!("Slint '{backend}' event loop failed: {e}"))?;
    drop(scene);
    drop(vt);
    match failure.take() {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

fn gl_loader(api: &slint::GraphicsAPI<'_>) -> Option<glow::Context> {
    let slint::GraphicsAPI::NativeOpenGL { get_proc_address } = api else { return None };
    // SAFETY: called with the context current (inside a rendering notification).
    Some(unsafe { glow::Context::from_loader_function_cstr(|name| get_proc_address(name)) })
}

/// GL_VENDOR / GL_RENDERER / GL_VERSION, to prove which driver rendered.
fn gl_info(api: &slint::GraphicsAPI<'_>) -> serde_json::Value {
    use glow::HasContext;
    let Some(gl) = gl_loader(api) else { return json!(format!("{api:?}")) };
    // SAFETY: plain queries on the current context.
    unsafe {
        let info = json!({
            "vendor": gl.get_parameter_string(glow::VENDOR),
            "renderer": gl.get_parameter_string(glow::RENDERER),
            "version": gl.get_parameter_string(glow::VERSION),
            "max_texture_size": gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE),
        });
        eprintln!("spike-gpu: GL {info}");
        info
    }
}

/// Decodes every frame and uploads it into its own RGBA texture; the decoded
/// pixels are dropped right after each upload, so RAM holds one frame at a time.
fn upload_textures(api: &slint::GraphicsAPI<'_>, anim: &Animation) -> Result<(Vec<Vec<Image>>, serde_json::Value), String> {
    use glow::HasContext;
    let gl = gl_loader(api).ok_or("the renderer does not expose OpenGL")?;
    let t0 = Instant::now();
    let (mut decode_ms, mut upload_ms) = (0.0, 0.0);
    let (w, h) = (anim.width as i32, anim.height as i32);
    let mut out = Vec::new();
    // SAFETY: GL calls on the current context; the texture binding and unpack
    // alignment femtovg relies on are restored before returning.
    unsafe {
        let max = gl.get_parameter_i32(glow::MAX_TEXTURE_SIZE);
        if w > max || h > max {
            return Err(format!("frames are {w}x{h}, GL_MAX_TEXTURE_SIZE is {max}"));
        }
        let prev_tex = gl.get_parameter_i32(glow::TEXTURE_BINDING_2D);
        let prev_align = gl.get_parameter_i32(glow::UNPACK_ALIGNMENT);
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
        for pose in &anim.poses {
            let mut v = Vec::with_capacity(pose.pngs.len());
            for png in &pose.pngs {
                let td = Instant::now();
                let rgba = frames::rgb_to_rgba(&frames::decode_rgb(png)?);
                let tu = Instant::now();
                decode_ms += (tu - td).as_secs_f64() * 1e3;
                let tex = gl.create_texture()?;
                gl.bind_texture(glow::TEXTURE_2D, Some(tex));
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
                gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::RGBA as i32,
                    w,
                    h,
                    0,
                    glow::RGBA,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(&rgba)),
                );
                upload_ms += tu.elapsed().as_secs_f64() * 1e3;
                let img = slint::BorrowedOpenGLTextureBuilder::new_gl_2d_rgba_texture(
                    tex.0,
                    [anim.width, anim.height].into(),
                )
                .build();
                v.push(img);
            }
            out.push(v);
        }
        let tf = Instant::now();
        gl.finish();
        upload_ms += tf.elapsed().as_secs_f64() * 1e3;
        let err = gl.get_error();
        gl.bind_texture(glow::TEXTURE_2D, std::num::NonZeroU32::new(prev_tex as u32).map(glow::NativeTexture));
        gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, prev_align);
        if err != glow::NO_ERROR {
            return Err(format!("GL error 0x{err:x} while uploading textures"));
        }
    }
    let n = anim.frame_count();
    let mb = (n * (anim.width * anim.height * 4) as usize) as f64 / 1048576.0;
    let meta = json!({
        "textures": n,
        "texture_mb_rgba": (mb * 10.0).round() / 10.0,
        "decode_ms": (decode_ms * 10.0f64).round() / 10.0,
        "upload_ms": (upload_ms * 10.0f64).round() / 10.0,
        "total_ms": (t0.elapsed().as_secs_f64() * 1e4).round() / 10.0,
    });
    eprintln!("spike-gpu: uploaded {meta}");
    Ok((out, meta))
}
