// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

// Saves a frame to PNG the way the body will read it: the dma-buf is imported as an EGLImage
// (EGL_EXT_image_dma_buf_import, with the modifier when there is one), bound to a GLES texture and read
// back through a framebuffer. Debugging aid only: it runs on the helper's main thread.

#include "snapshot.h"

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES2/gl2.h>
#include <GLES2/gl2ext.h>
#include <drm_fourcc.h>
#include <png.h>
#include <stdio.h>

static EGLContext context = EGL_NO_CONTEXT;

static gboolean ensure_context(EGLDisplay egl, GError **error)
{
    if (context != EGL_NO_CONTEXT)
        return TRUE;
    if (!eglBindAPI(EGL_OPENGL_ES_API)) {
        g_set_error_literal(error, G_IO_ERROR, G_IO_ERROR_FAILED, "eglBindAPI failed");
        return FALSE;
    }
    static const EGLint attribs[] = {EGL_CONTEXT_CLIENT_VERSION, 2, EGL_NONE};
    context = eglCreateContext(egl, EGL_NO_CONFIG_KHR, EGL_NO_CONTEXT, attribs);
    if (context == EGL_NO_CONTEXT) {
        g_set_error(error, G_IO_ERROR, G_IO_ERROR_FAILED, "eglCreateContext failed: %#x", eglGetError());
        return FALSE;
    }
    return TRUE;
}

static EGLImageKHR import_frame(EGLDisplay egl, const HcFrame *frame)
{
    static const EGLint fd_attr[] = {EGL_DMA_BUF_PLANE0_FD_EXT, EGL_DMA_BUF_PLANE1_FD_EXT,
                                     EGL_DMA_BUF_PLANE2_FD_EXT, EGL_DMA_BUF_PLANE3_FD_EXT};
    static const EGLint offset_attr[] = {EGL_DMA_BUF_PLANE0_OFFSET_EXT, EGL_DMA_BUF_PLANE1_OFFSET_EXT,
                                         EGL_DMA_BUF_PLANE2_OFFSET_EXT, EGL_DMA_BUF_PLANE3_OFFSET_EXT};
    static const EGLint pitch_attr[] = {EGL_DMA_BUF_PLANE0_PITCH_EXT, EGL_DMA_BUF_PLANE1_PITCH_EXT,
                                        EGL_DMA_BUF_PLANE2_PITCH_EXT, EGL_DMA_BUF_PLANE3_PITCH_EXT};
    static const EGLint mod_lo_attr[] = {EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, EGL_DMA_BUF_PLANE1_MODIFIER_LO_EXT,
                                         EGL_DMA_BUF_PLANE2_MODIFIER_LO_EXT, EGL_DMA_BUF_PLANE3_MODIFIER_LO_EXT};
    static const EGLint mod_hi_attr[] = {EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, EGL_DMA_BUF_PLANE1_MODIFIER_HI_EXT,
                                         EGL_DMA_BUF_PLANE2_MODIFIER_HI_EXT, EGL_DMA_BUF_PLANE3_MODIFIER_HI_EXT};
    EGLint attribs[64];
    int n = 0;
    attribs[n++] = EGL_WIDTH;
    attribs[n++] = frame->width;
    attribs[n++] = EGL_HEIGHT;
    attribs[n++] = frame->height;
    attribs[n++] = EGL_LINUX_DRM_FOURCC_EXT;
    attribs[n++] = (EGLint)frame->fourcc;
    for (guint32 i = 0; i < frame->n_planes; i++) {
        attribs[n++] = fd_attr[i];
        attribs[n++] = frame->fds[i];
        attribs[n++] = offset_attr[i];
        attribs[n++] = (EGLint)frame->offsets[i];
        attribs[n++] = pitch_attr[i];
        attribs[n++] = (EGLint)frame->strides[i];
        if (frame->modifier != DRM_FORMAT_MOD_INVALID) {
            attribs[n++] = mod_lo_attr[i];
            attribs[n++] = (EGLint)(frame->modifier & 0xffffffff);
            attribs[n++] = mod_hi_attr[i];
            attribs[n++] = (EGLint)(frame->modifier >> 32);
        }
    }
    attribs[n++] = EGL_NONE;
    PFNEGLCREATEIMAGEKHRPROC create_image = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
    return create_image(egl, EGL_NO_CONTEXT, EGL_LINUX_DMA_BUF_EXT, NULL, attribs);
}

static gboolean write_png(const char *path, const guint8 *rgba, int width, int height, GError **error)
{
    FILE *file = fopen(path, "wb");
    if (!file) {
        g_set_error(error, G_IO_ERROR, G_IO_ERROR_FAILED, "cannot write %s", path);
        return FALSE;
    }
    png_structp png = png_create_write_struct(PNG_LIBPNG_VER_STRING, NULL, NULL, NULL);
    png_infop info = png_create_info_struct(png);
    if (setjmp(png_jmpbuf(png))) {
        png_destroy_write_struct(&png, &info);
        fclose(file);
        g_set_error(error, G_IO_ERROR, G_IO_ERROR_FAILED, "libpng failed on %s", path);
        return FALSE;
    }
    png_init_io(png, file);
    png_set_IHDR(png, info, (png_uint_32)width, (png_uint_32)height, 8, PNG_COLOR_TYPE_RGB_ALPHA, PNG_INTERLACE_NONE,
                 PNG_COMPRESSION_TYPE_DEFAULT, PNG_FILTER_TYPE_DEFAULT);
    png_write_info(png, info);
    // Read through a framebuffer, GL's row 0 is the texture's first row: the top of the page.
    for (int y = 0; y < height; y++)
        png_write_row(png, (png_const_bytep)(rgba + (size_t)y * (size_t)width * 4));
    png_write_end(png, NULL);
    png_destroy_write_struct(&png, &info);
    fclose(file);
    return TRUE;
}

gboolean hc_snapshot_png(WPEDisplay *display, const HcFrame *frame, const char *path, GError **error)
{
    EGLDisplay egl = wpe_display_get_egl_display(display, error);
    if (!egl)
        return FALSE;
    if (!ensure_context(egl, error))
        return FALSE;

    EGLDisplay old_display = eglGetCurrentDisplay();
    EGLContext old_context = eglGetCurrentContext();
    EGLSurface old_draw = eglGetCurrentSurface(EGL_DRAW);
    EGLSurface old_read = eglGetCurrentSurface(EGL_READ);
    if (!eglMakeCurrent(egl, EGL_NO_SURFACE, EGL_NO_SURFACE, context)) {
        g_set_error(error, G_IO_ERROR, G_IO_ERROR_FAILED, "eglMakeCurrent failed: %#x", eglGetError());
        return FALSE;
    }

    gboolean ok = FALSE;
    guint8 *pixels = NULL;
    GLuint texture = 0, fbo = 0;
    EGLImageKHR image = import_frame(egl, frame);
    if (image == EGL_NO_IMAGE_KHR) {
        g_set_error(error, G_IO_ERROR, G_IO_ERROR_FAILED, "dma-buf import failed: %#x", eglGetError());
        goto out;
    }
    PFNGLEGLIMAGETARGETTEXTURE2DOESPROC target_texture =
        (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
    glGenTextures(1, &texture);
    glBindTexture(GL_TEXTURE_2D, texture);
    target_texture(GL_TEXTURE_2D, image);
    glGenFramebuffers(1, &fbo);
    glBindFramebuffer(GL_FRAMEBUFFER, fbo);
    glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, texture, 0);
    if (glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE) {
        g_set_error_literal(error, G_IO_ERROR, G_IO_ERROR_FAILED, "framebuffer incomplete");
        goto out;
    }
    pixels = g_malloc((size_t)frame->width * (size_t)frame->height * 4);
    glReadPixels(0, 0, frame->width, frame->height, GL_RGBA, GL_UNSIGNED_BYTE, pixels);
    if (frame->fourcc == DRM_FORMAT_XRGB8888 || frame->fourcc == DRM_FORMAT_XBGR8888)
        for (size_t i = 3; i < (size_t)frame->width * (size_t)frame->height * 4; i += 4)
            pixels[i] = 0xff;
    ok = write_png(path, pixels, frame->width, frame->height, error);

out:
    g_free(pixels);
    if (fbo)
        glDeleteFramebuffers(1, &fbo);
    if (texture)
        glDeleteTextures(1, &texture);
    if (image != EGL_NO_IMAGE_KHR) {
        PFNEGLDESTROYIMAGEKHRPROC destroy_image = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR");
        destroy_image(egl, image);
    }
    if (old_context != EGL_NO_CONTEXT)
        eglMakeCurrent(old_display, old_draw, old_read, old_context);
    else
        eglMakeCurrent(egl, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
    return ok;
}
