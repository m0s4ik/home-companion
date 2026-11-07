// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! EGL and GLES 2 loaded with dlopen (libEGL.so.1, libGLESv2.so.2): nothing linked, so the
//! binary also starts where Mesa lives outside the default library path (found through
//! `ld.so.conf.d` or `LD_LIBRARY_PATH`). Shared by the viewer and the fake producer (the
//! producer includes this file with `#[path]`).

#![allow(non_snake_case, clippy::upper_case_acronyms, dead_code)]

use std::ffi::{CStr, CString, c_char, c_void};
use std::os::fd::RawFd;

pub type EGLDisplay = *mut c_void;
pub type EGLConfig = *mut c_void;
pub type EGLContext = *mut c_void;
pub type EGLSurface = *mut c_void;
pub type EGLImage = *mut c_void;
pub type EGLint = i32;
pub type EGLBoolean = u32;
pub type EGLenum = u32;
pub type GLuint = u32;
pub type GLint = i32;
pub type GLenum = u32;
pub type GLsizei = i32;
pub type GLfloat = f32;

pub const EGL_NONE: EGLint = 0x3038;
pub const EGL_PLATFORM_GBM_KHR: EGLenum = 0x31D7;
pub const EGL_OPENGL_ES_API: EGLenum = 0x30A0;
pub const EGL_SURFACE_TYPE: EGLint = 0x3033;
pub const EGL_WINDOW_BIT: EGLint = 4;
pub const EGL_RENDERABLE_TYPE: EGLint = 0x3040;
pub const EGL_OPENGL_ES2_BIT: EGLint = 4;
pub const EGL_RED_SIZE: EGLint = 0x3024;
pub const EGL_GREEN_SIZE: EGLint = 0x3023;
pub const EGL_BLUE_SIZE: EGLint = 0x3022;
pub const EGL_ALPHA_SIZE: EGLint = 0x3021;
pub const EGL_NATIVE_VISUAL_ID: EGLint = 0x302E;
pub const EGL_CONTEXT_CLIENT_VERSION: EGLint = 0x3098;
pub const EGL_VENDOR: EGLint = 0x3053;
pub const EGL_VERSION: EGLint = 0x3054;
pub const EGL_EXTENSIONS: EGLint = 0x3055;
pub const EGL_WIDTH: EGLint = 0x3057;
pub const EGL_HEIGHT: EGLint = 0x3056;
pub const EGL_LINUX_DMA_BUF_EXT: EGLenum = 0x3270;
pub const EGL_LINUX_DRM_FOURCC_EXT: EGLint = 0x3271;
pub const EGL_IMAGE_PRESERVED_KHR: EGLint = 0x30D2;
pub const EGL_TRUE: EGLint = 1;
/// Per plane: fd, offset, pitch, modifier lo, modifier hi.
const PLANE_ATTRS: [[EGLint; 5]; 4] = [
    [0x3272, 0x3273, 0x3274, 0x3443, 0x3444],
    [0x3275, 0x3276, 0x3277, 0x3445, 0x3446],
    [0x3278, 0x3279, 0x327A, 0x3447, 0x3448],
    [0x3440, 0x3441, 0x3442, 0x3449, 0x344A],
];

pub const GL_TEXTURE_2D: GLenum = 0x0DE1;
pub const GL_TEXTURE_MIN_FILTER: GLenum = 0x2801;
pub const GL_TEXTURE_MAG_FILTER: GLenum = 0x2800;
pub const GL_TEXTURE_WRAP_S: GLenum = 0x2802;
pub const GL_TEXTURE_WRAP_T: GLenum = 0x2803;
pub const GL_LINEAR: GLint = 0x2601;
pub const GL_NEAREST: GLint = 0x2600;
pub const GL_CLAMP_TO_EDGE: GLint = 0x812F;
pub const GL_FRAGMENT_SHADER: GLenum = 0x8B30;
pub const GL_VERTEX_SHADER: GLenum = 0x8B31;
pub const GL_COMPILE_STATUS: GLenum = 0x8B81;
pub const GL_LINK_STATUS: GLenum = 0x8B82;
pub const GL_TRIANGLE_STRIP: GLenum = 5;
pub const GL_FLOAT: GLenum = 0x1406;
pub const GL_COLOR_BUFFER_BIT: GLenum = 0x4000;
pub const GL_RGBA: GLenum = 0x1908;
pub const GL_UNSIGNED_BYTE: GLenum = 0x1401;
pub const GL_FRAMEBUFFER: GLenum = 0x8D40;
pub const GL_COLOR_ATTACHMENT0: GLenum = 0x8CE0;
pub const GL_FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;
pub const GL_TEXTURE0: GLenum = 0x84C0;
pub const GL_RENDERER: GLenum = 0x1F01;
pub const GL_TEXTURE_EXTERNAL_OES: GLenum = 0x8D65;
pub const GL_FRAMEBUFFER_BINDING: GLenum = 0x8CA6;
pub const GL_VIEWPORT: GLenum = 0x0BA2;
pub const GL_ARRAY_BUFFER: GLenum = 0x8892;
pub const GL_SCISSOR_TEST: GLenum = 0x0C11;
pub const GL_STENCIL_TEST: GLenum = 0x0B90;
pub const GL_CULL_FACE: GLenum = 0x0B44;
pub const EGL_YUV_COLOR_SPACE_HINT_EXT: EGLint = 0x327B;
pub const EGL_SAMPLE_RANGE_HINT_EXT: EGLint = 0x327C;
pub const EGL_ITU_REC601_EXT: EGLint = 0x327F;
pub const EGL_ITU_REC709_EXT: EGLint = 0x3280;
pub const EGL_YUV_FULL_RANGE_EXT: EGLint = 0x3282;
pub const EGL_YUV_NARROW_RANGE_EXT: EGLint = 0x3283;
pub const GL_VERSION: GLenum = 0x1F02;
pub const GL_EXTENSIONS: GLenum = 0x1F03;
pub const GL_BLEND: GLenum = 0x0BE2;

/// A table of C function pointers resolved by name.
macro_rules! api {
    ($table:ident { $( fn $f:ident ( $($a:ty),* ) $(-> $r:ty)? ; )* }) => {
        pub struct $table {
            $( pub $f: unsafe extern "C" fn($($a),*) $(-> $r)?, )*
        }
        impl $table {
            /// # Safety
            /// `get` must return a pointer to a function with the declared signature, or null.
            unsafe fn load(get: &dyn Fn(&CStr) -> *mut c_void) -> Result<Self, String> {
                Ok(Self {
                    $( $f: {
                        let name = concat!(stringify!($f), "\0");
                        let p = get(CStr::from_bytes_with_nul(name.as_bytes()).unwrap());
                        if p.is_null() {
                            return Err(format!("missing {}", stringify!($f)));
                        }
                        // SAFETY: by the caller's contract.
                        unsafe { std::mem::transmute::<*mut c_void, unsafe extern "C" fn($($a),*) $(-> $r)?>(p) }
                    }, )*
                })
            }
        }
    };
}

api!(Egl {
    fn eglGetError() -> EGLint;
    fn eglGetProcAddress(*const c_char) -> *mut c_void;
    fn eglQueryString(EGLDisplay, EGLint) -> *const c_char;
    fn eglInitialize(EGLDisplay, *mut EGLint, *mut EGLint) -> EGLBoolean;
    fn eglTerminate(EGLDisplay) -> EGLBoolean;
    fn eglBindAPI(EGLenum) -> EGLBoolean;
    fn eglChooseConfig(EGLDisplay, *const EGLint, *mut EGLConfig, EGLint, *mut EGLint) -> EGLBoolean;
    fn eglGetConfigAttrib(EGLDisplay, EGLConfig, EGLint, *mut EGLint) -> EGLBoolean;
    fn eglCreateContext(EGLDisplay, EGLConfig, EGLContext, *const EGLint) -> EGLContext;
    fn eglDestroyContext(EGLDisplay, EGLContext) -> EGLBoolean;
    fn eglCreateWindowSurface(EGLDisplay, EGLConfig, *mut c_void, *const EGLint) -> EGLSurface;
    fn eglDestroySurface(EGLDisplay, EGLSurface) -> EGLBoolean;
    fn eglMakeCurrent(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> EGLBoolean;
    fn eglSwapBuffers(EGLDisplay, EGLSurface) -> EGLBoolean;
    fn eglSwapInterval(EGLDisplay, EGLint) -> EGLBoolean;
});

api!(EglExt {
    fn eglGetPlatformDisplayEXT(EGLenum, *mut c_void, *const EGLint) -> EGLDisplay;
    fn eglCreateImageKHR(EGLDisplay, EGLContext, EGLenum, *mut c_void, *const EGLint) -> EGLImage;
    fn eglDestroyImageKHR(EGLDisplay, EGLImage) -> EGLBoolean;
});

api!(DmaBufQuery {
    fn eglQueryDmaBufFormatsEXT(EGLDisplay, EGLint, *mut EGLint, *mut EGLint) -> EGLBoolean;
    fn eglQueryDmaBufModifiersEXT(EGLDisplay, EGLint, EGLint, *mut u64, *mut EGLBoolean, *mut EGLint) -> EGLBoolean;
});

api!(Gl {
    fn glGetError() -> GLenum;
    fn glGetString(GLenum) -> *const c_char;
    fn glViewport(GLint, GLint, GLsizei, GLsizei);
    fn glClearColor(GLfloat, GLfloat, GLfloat, GLfloat);
    fn glClear(GLenum);
    fn glDisable(GLenum);
    fn glFlush();
    fn glFinish();
    fn glCreateShader(GLenum) -> GLuint;
    fn glShaderSource(GLuint, GLsizei, *const *const c_char, *const GLint);
    fn glCompileShader(GLuint);
    fn glGetShaderiv(GLuint, GLenum, *mut GLint);
    fn glGetShaderInfoLog(GLuint, GLsizei, *mut GLsizei, *mut c_char);
    fn glCreateProgram() -> GLuint;
    fn glAttachShader(GLuint, GLuint);
    fn glBindAttribLocation(GLuint, GLuint, *const c_char);
    fn glLinkProgram(GLuint);
    fn glGetProgramiv(GLuint, GLenum, *mut GLint);
    fn glGetProgramInfoLog(GLuint, GLsizei, *mut GLsizei, *mut c_char);
    fn glUseProgram(GLuint);
    fn glGetUniformLocation(GLuint, *const c_char) -> GLint;
    fn glUniform1i(GLint, GLint);
    fn glUniform1f(GLint, GLfloat);
    fn glUniform2f(GLint, GLfloat, GLfloat);
    fn glGenTextures(GLsizei, *mut GLuint);
    fn glDeleteTextures(GLsizei, *const GLuint);
    fn glBindTexture(GLenum, GLuint);
    fn glActiveTexture(GLenum);
    fn glTexParameteri(GLenum, GLenum, GLint);
    fn glVertexAttribPointer(GLuint, GLint, GLenum, u8, GLsizei, *const c_void);
    fn glEnableVertexAttribArray(GLuint);
    fn glDrawArrays(GLenum, GLint, GLsizei);
    fn glReadPixels(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, *mut c_void);
    fn glGenFramebuffers(GLsizei, *mut GLuint);
    fn glDeleteFramebuffers(GLsizei, *const GLuint);
    fn glBindFramebuffer(GLenum, GLuint);
    fn glFramebufferTexture2D(GLenum, GLenum, GLenum, GLuint, GLint);
    fn glCheckFramebufferStatus(GLenum) -> GLenum;
    fn glEGLImageTargetTexture2DOES(GLenum, EGLImage);
    fn glGetIntegerv(GLenum, *mut GLint);
    fn glTexImage2D(GLenum, GLint, GLint, GLsizei, GLsizei, GLint, GLenum, GLenum, *const c_void);
    fn glBindBuffer(GLenum, GLuint);
    fn glDisableVertexAttribArray(GLuint);
    fn glEnable(GLenum);
});

/// The loaded libraries (kept open for the life of the process).
pub struct Api {
    pub egl: Egl,
    pub ext: EglExt,
    pub query: Option<DmaBufQuery>,
    pub gl: Gl,
}

fn dlopen(name: &CStr) -> Result<*mut c_void, String> {
    // SAFETY: dlopen with a valid C string.
    let h = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
    if h.is_null() {
        // SAFETY: dlerror returns a C string or null.
        let e = unsafe { libc::dlerror() };
        let why = if e.is_null() {
            String::new()
        } else {
            // SAFETY: non-null C string from dlerror.
            unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned()
        };
        return Err(format!("{}: {why}", name.to_string_lossy()));
    }
    Ok(h)
}

impl Api {
    pub fn load() -> Result<Api, String> {
        let libegl = dlopen(c"libEGL.so.1")?;
        let libgles = dlopen(c"libGLESv2.so.2")?;
        // SAFETY: symbols of libEGL resolved by their documented names and signatures.
        let egl = unsafe { Egl::load(&|n: &CStr| libc::dlsym(libegl, n.as_ptr())) }?;
        let gpa = egl.eglGetProcAddress;
        // SAFETY: extension entry points by name; null when missing.
        let proc = |n: &CStr| unsafe { gpa(n.as_ptr()) };
        // SAFETY: as above.
        let ext = unsafe { EglExt::load(&proc) }?;
        // SAFETY: as above.
        let query = unsafe { DmaBufQuery::load(&proc) }.ok();
        // SAFETY: core GLES symbols from libGLESv2, the OES extension through eglGetProcAddress.
        let gl = unsafe {
            Gl::load(&|n: &CStr| {
                let p = libc::dlsym(libgles, n.as_ptr());
                if p.is_null() { gpa(n.as_ptr()) } else { p }
            })
        }?;
        Ok(Api {
            egl,
            ext,
            query,
            gl,
        })
    }

    pub fn error(&self) -> String {
        // SAFETY: eglGetError has no preconditions.
        format!("EGL error 0x{:x}", unsafe { (self.egl.eglGetError)() })
    }

    pub fn query_string(&self, dpy: EGLDisplay, name: EGLint) -> String {
        // SAFETY: eglQueryString returns a static C string or null.
        let p = unsafe { (self.egl.eglQueryString)(dpy, name) };
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: non-null C string owned by EGL.
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    }

    pub fn gl_string(&self, name: GLenum) -> String {
        // SAFETY: needs a current context; returns a static C string or null.
        let p = unsafe { (self.gl.glGetString)(name) };
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: non-null C string owned by GL.
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    }
}

/// An initialised EGL display on a GBM device, with a GLES 2 context.
pub struct Display {
    pub dpy: EGLDisplay,
    pub ctx: EGLContext,
    pub config: EGLConfig,
}

impl Display {
    /// `native_visual`: the GBM format the config must match (for a window surface), or None
    /// for a context used without a surface (surfaceless).
    pub fn new(api: &Api, gbm: *mut c_void, native_visual: Option<u32>) -> Result<Display, String> {
        // SAFETY: EGL calls with valid arguments; failures are checked.
        unsafe {
            let dpy =
                (api.ext.eglGetPlatformDisplayEXT)(EGL_PLATFORM_GBM_KHR, gbm, std::ptr::null());
            if dpy.is_null() {
                return Err(format!("eglGetPlatformDisplayEXT: {}", api.error()));
            }
            let (mut maj, mut min) = (0, 0);
            if (api.egl.eglInitialize)(dpy, &mut maj, &mut min) == 0 {
                return Err(format!("eglInitialize: {}", api.error()));
            }
            (api.egl.eglBindAPI)(EGL_OPENGL_ES_API);
            let attrs = [
                EGL_SURFACE_TYPE,
                if native_visual.is_some() {
                    EGL_WINDOW_BIT
                } else {
                    0
                },
                EGL_RENDERABLE_TYPE,
                EGL_OPENGL_ES2_BIT,
                EGL_RED_SIZE,
                8,
                EGL_GREEN_SIZE,
                8,
                EGL_BLUE_SIZE,
                8,
                EGL_NONE,
            ];
            let mut configs = [std::ptr::null_mut(); 64];
            let mut n = 0;
            if (api.egl.eglChooseConfig)(dpy, attrs.as_ptr(), configs.as_mut_ptr(), 64, &mut n) == 0
                || n == 0
            {
                return Err(format!("eglChooseConfig: {}", api.error()));
            }
            let config = configs[..n as usize]
                .iter()
                .copied()
                .find(|&c| match native_visual {
                    None => true,
                    Some(v) => {
                        let mut id = 0;
                        (api.egl.eglGetConfigAttrib)(dpy, c, EGL_NATIVE_VISUAL_ID, &mut id);
                        id as u32 == v
                    }
                })
                .ok_or("no EGL config for the GBM format")?;
            let ctx_attrs = [EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE];
            let ctx =
                (api.egl.eglCreateContext)(dpy, config, std::ptr::null_mut(), ctx_attrs.as_ptr());
            if ctx.is_null() {
                return Err(format!("eglCreateContext: {}", api.error()));
            }
            Ok(Display { dpy, ctx, config })
        }
    }
}

/// One dma-buf plane for `import_dmabuf`.
#[derive(Clone, Copy)]
pub struct PlaneFd {
    pub fd: RawFd,
    pub offset: u32,
    pub stride: u32,
}

/// `eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)`. No modifier attributes when the modifier is
/// `DRM_FORMAT_MOD_INVALID` (implicit layout). EGL dups the fds: the caller may close them.
pub fn import_dmabuf(
    api: &Api,
    dpy: EGLDisplay,
    width: u32,
    height: u32,
    fourcc: u32,
    modifier: u64,
    planes: &[PlaneFd],
) -> Result<EGLImage, String> {
    let mut a = vec![
        EGL_WIDTH,
        width as EGLint,
        EGL_HEIGHT,
        height as EGLint,
        EGL_LINUX_DRM_FOURCC_EXT,
        fourcc as EGLint,
    ];
    for (p, names) in planes.iter().zip(PLANE_ATTRS) {
        a.extend_from_slice(&[
            names[0],
            p.fd,
            names[1],
            p.offset as EGLint,
            names[2],
            p.stride as EGLint,
        ]);
        if modifier != webview_proto::MOD_INVALID {
            a.extend_from_slice(&[
                names[3],
                (modifier & 0xffff_ffff) as u32 as EGLint,
                names[4],
                (modifier >> 32) as u32 as EGLint,
            ]);
        }
    }
    a.extend_from_slice(&[EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE]);
    // SAFETY: attribute list terminated by EGL_NONE; EGL_NO_CONTEXT as the spec requires for
    // dma-buf imports.
    let img = unsafe {
        (api.ext.eglCreateImageKHR)(
            dpy,
            std::ptr::null_mut(),
            EGL_LINUX_DMA_BUF_EXT,
            std::ptr::null_mut(),
            a.as_ptr(),
        )
    };
    if img.is_null() {
        Err(format!("eglCreateImageKHR(dma-buf): {}", api.error()))
    } else {
        Ok(img)
    }
}

/// A `GL_TEXTURE_2D` backed by the image (zero copy). Needs a current context.
pub fn texture_from_image(api: &Api, image: EGLImage) -> Result<GLuint, String> {
    let gl = &api.gl;
    // SAFETY: GL calls on the current context.
    unsafe {
        let mut tex = 0;
        (gl.glGenTextures)(1, &mut tex);
        (gl.glBindTexture)(GL_TEXTURE_2D, tex);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        (gl.glTexParameteri)(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        (gl.glGetError)();
        (gl.glEGLImageTargetTexture2DOES)(GL_TEXTURE_2D, image);
        let e = (gl.glGetError)();
        if e != 0 {
            (gl.glDeleteTextures)(1, &tex);
            return Err(format!("glEGLImageTargetTexture2DOES: GL error 0x{e:x}"));
        }
        Ok(tex)
    }
}

/// Compiles and links a program; attribute `i` of `attribs` gets location `i`.
pub fn program(api: &Api, vs: &str, fs: &str, attribs: &[&str]) -> Result<GLuint, String> {
    let gl = &api.gl;
    // SAFETY: GL calls on the current context with valid C strings.
    unsafe {
        let shader = |kind, src: &str| -> Result<GLuint, String> {
            let s = (gl.glCreateShader)(kind);
            let c = CString::new(src).unwrap();
            let p = c.as_ptr();
            (gl.glShaderSource)(s, 1, &p, std::ptr::null());
            (gl.glCompileShader)(s);
            let mut ok = 0;
            (gl.glGetShaderiv)(s, GL_COMPILE_STATUS, &mut ok);
            if ok == 0 {
                let mut log = [0 as c_char; 1024];
                (gl.glGetShaderInfoLog)(s, 1024, std::ptr::null_mut(), log.as_mut_ptr());
                return Err(format!(
                    "shader: {}",
                    CStr::from_ptr(log.as_ptr()).to_string_lossy()
                ));
            }
            Ok(s)
        };
        let p = (gl.glCreateProgram)();
        (gl.glAttachShader)(p, shader(GL_VERTEX_SHADER, vs)?);
        (gl.glAttachShader)(p, shader(GL_FRAGMENT_SHADER, fs)?);
        for (i, a) in attribs.iter().enumerate() {
            let c = CString::new(*a).unwrap();
            (gl.glBindAttribLocation)(p, i as GLuint, c.as_ptr());
        }
        (gl.glLinkProgram)(p);
        let mut ok = 0;
        (gl.glGetProgramiv)(p, GL_LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut log = [0 as c_char; 1024];
            (gl.glGetProgramInfoLog)(p, 1024, std::ptr::null_mut(), log.as_mut_ptr());
            return Err(format!(
                "link: {}",
                CStr::from_ptr(log.as_ptr()).to_string_lossy()
            ));
        }
        Ok(p)
    }
}

/// The formats and modifiers this EGL can import (for logs and the protocol docs).
pub fn dmabuf_formats(api: &Api, dpy: EGLDisplay) -> Vec<(u32, Vec<(u64, bool)>)> {
    let Some(q) = &api.query else {
        return Vec::new();
    };
    let mut out = Vec::new();
    // SAFETY: two-call pattern of EGL_EXT_image_dma_buf_import_modifiers.
    unsafe {
        let mut n = 0;
        (q.eglQueryDmaBufFormatsEXT)(dpy, 0, std::ptr::null_mut(), &mut n);
        let mut formats = vec![0; n as usize];
        (q.eglQueryDmaBufFormatsEXT)(dpy, n, formats.as_mut_ptr(), &mut n);
        for f in formats {
            let mut m = 0;
            (q.eglQueryDmaBufModifiersEXT)(
                dpy,
                f,
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut m,
            );
            let mut mods = vec![0u64; m as usize];
            let mut ext = vec![0u32; m as usize];
            (q.eglQueryDmaBufModifiersEXT)(dpy, f, m, mods.as_mut_ptr(), ext.as_mut_ptr(), &mut m);
            out.push((
                f as u32,
                mods.into_iter()
                    .zip(ext.into_iter().map(|e| e != 0))
                    .collect(),
            ));
        }
    }
    out
}

/// "AR24" for a fourcc.
pub fn fourcc_name(f: u32) -> String {
    f.to_le_bytes().iter().map(|&b| b as char).collect()
}
