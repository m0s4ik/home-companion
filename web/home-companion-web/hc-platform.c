// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

#include "hc-platform.h"

#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <fcntl.h>
#include <gbm.h>
#include <sys/stat.h>
#include <unistd.h>

// ---------------------------------------------------------------------------------------------- display

struct _HcDisplay {
    WPEDisplay parent_instance;
    char *render_node;
    WPEDRMDevice *drm_device;
    int gbm_fd;
    struct gbm_device *gbm;
};

G_DEFINE_FINAL_TYPE(HcDisplay, hc_display, WPE_TYPE_DISPLAY)

static gboolean hc_display_connect(WPEDisplay *display, GError **error)
{
    HcDisplay *self = HC_DISPLAY(display);
    if (access(self->render_node, R_OK | W_OK) != 0) {
        g_set_error(error, WPE_DISPLAY_ERROR, WPE_DISPLAY_ERROR_CONNECTION_FAILED, "cannot open %s",
                    self->render_node);
        return FALSE;
    }
    return TRUE;
}

static WPEView *hc_display_create_view(WPEDisplay *display)
{
    return WPE_VIEW(g_object_new(HC_TYPE_VIEW, "display", display, NULL));
}

static WPEToplevel *hc_display_create_toplevel(WPEDisplay *display, guint max_views)
{
    (void)max_views;
    return WPE_TOPLEVEL(g_object_new(HC_TYPE_TOPLEVEL, "display", display, NULL));
}

// The UI-process EGL display: WPEPlatform builds the preferred buffer formats from it, and the helper
// imports buffers with it (PNG snapshots). GBM on the render node, as WPE's headless platform does.
static gpointer hc_display_get_egl_display(WPEDisplay *display, GError **error)
{
    HcDisplay *self = HC_DISPLAY(display);
    if (!self->gbm) {
        self->gbm_fd = open(self->render_node, O_RDWR | O_CLOEXEC);
        if (self->gbm_fd < 0) {
            g_set_error(error, WPE_EGL_ERROR, WPE_EGL_ERROR_NOT_AVAILABLE, "cannot open %s", self->render_node);
            return NULL;
        }
        self->gbm = gbm_create_device(self->gbm_fd);
        if (!self->gbm) {
            g_set_error(error, WPE_EGL_ERROR, WPE_EGL_ERROR_NOT_AVAILABLE, "gbm_create_device failed");
            return NULL;
        }
    }
    EGLDisplay egl = eglGetPlatformDisplay(EGL_PLATFORM_GBM_KHR, self->gbm, NULL);
    if (egl == EGL_NO_DISPLAY) {
        g_set_error(error, WPE_EGL_ERROR, WPE_EGL_ERROR_NOT_AVAILABLE, "no GBM EGL display");
        return NULL;
    }
    return egl;
}

static WPEDRMDevice *hc_display_get_drm_device(WPEDisplay *display)
{
    return HC_DISPLAY(display)->drm_device;
}

static void hc_display_finalize(GObject *object)
{
    HcDisplay *self = HC_DISPLAY(object);
    g_clear_pointer(&self->gbm, gbm_device_destroy);
    if (self->gbm_fd >= 0)
        close(self->gbm_fd);
    g_clear_pointer(&self->drm_device, wpe_drm_device_unref);
    g_free(self->render_node);
    G_OBJECT_CLASS(hc_display_parent_class)->finalize(object);
}

static void hc_display_class_init(HcDisplayClass *klass)
{
    G_OBJECT_CLASS(klass)->finalize = hc_display_finalize;
    WPEDisplayClass *display_class = WPE_DISPLAY_CLASS(klass);
    display_class->connect = hc_display_connect;
    display_class->create_view = hc_display_create_view;
    display_class->create_toplevel = hc_display_create_toplevel;
    display_class->get_egl_display = hc_display_get_egl_display;
    display_class->get_drm_device = hc_display_get_drm_device;
}

static void hc_display_init(HcDisplay *self)
{
    self->gbm_fd = -1;
}

WPEDisplay *hc_display_new(const char *render_node, const char *primary_node, GError **error)
{
    HcDisplay *self = g_object_new(HC_TYPE_DISPLAY, NULL);
    self->render_node = g_strdup(render_node);
    self->drm_device = wpe_drm_device_new(primary_node, render_node);
    if (!wpe_display_connect(WPE_DISPLAY(self), error)) {
        g_object_unref(self);
        return NULL;
    }
    return WPE_DISPLAY(self);
}

// --------------------------------------------------------------------------------------------- toplevel

struct _HcToplevel {
    WPEToplevel parent_instance;
};

G_DEFINE_FINAL_TYPE(HcToplevel, hc_toplevel, WPE_TYPE_TOPLEVEL)

static gboolean resize_view(WPEToplevel *toplevel, WPEView *view, gpointer user_data)
{
    (void)user_data;
    int width, height;
    wpe_toplevel_get_size(toplevel, &width, &height);
    wpe_view_resized(view, width, height);
    return FALSE;
}

static gboolean hc_toplevel_resize(WPEToplevel *toplevel, int width, int height)
{
    wpe_toplevel_resized(toplevel, width, height);
    wpe_toplevel_foreach_view(toplevel, resize_view, NULL);
    return TRUE;
}

static void hc_toplevel_constructed(GObject *object)
{
    G_OBJECT_CLASS(hc_toplevel_parent_class)->constructed(object);
    wpe_toplevel_state_changed(WPE_TOPLEVEL(object), WPE_TOPLEVEL_STATE_ACTIVE | WPE_TOPLEVEL_STATE_FULLSCREEN);
}

static void hc_toplevel_class_init(HcToplevelClass *klass)
{
    G_OBJECT_CLASS(klass)->constructed = hc_toplevel_constructed;
    WPE_TOPLEVEL_CLASS(klass)->resize = hc_toplevel_resize;
}

static void hc_toplevel_init(HcToplevel *self)
{
    (void)self;
}

void hc_toplevel_configure(HcToplevel *toplevel, int logical_width, int logical_height, double scale)
{
    wpe_toplevel_scale_changed(WPE_TOPLEVEL(toplevel), scale);
    wpe_toplevel_resize(WPE_TOPLEVEL(toplevel), logical_width, logical_height);
}

// ------------------------------------------------------------------------------------------------- view

typedef struct {
    guint32 id;
    gboolean announced;
    GWeakRef view;
} BufferInfo;

struct _HcView {
    WPEView parent_instance;
    const HcFrameSink *sink;
    gpointer sink_data;
    guint32 next_id;
    GHashTable *held;       // id -> WPEBuffer (reference held while the consumer may sample it)
    WPEBuffer *pending;     // the frame waiting for hc_view_frame_done() (reference)
};

G_DEFINE_FINAL_TYPE(HcView, hc_view, WPE_TYPE_VIEW)

static GQuark buffer_info_quark(void)
{
    return g_quark_from_static_string("hc-buffer-info");
}

static void buffer_info_free(gpointer data)
{
    BufferInfo *info = data;
    HcView *view = g_weak_ref_get(&info->view);
    if (view) {
        if (view->sink && view->sink->drop)
            view->sink->drop(view, info->id, view->sink_data);
        g_object_unref(view);
    }
    g_weak_ref_clear(&info->view);
    g_free(info);
}

static BufferInfo *buffer_info(HcView *view, WPEBuffer *buffer)
{
    BufferInfo *info = g_object_get_qdata(G_OBJECT(buffer), buffer_info_quark());
    if (!info) {
        info = g_new0(BufferInfo, 1);
        info->id = ++view->next_id;
        g_weak_ref_init(&info->view, view);
        g_object_set_qdata_full(G_OBJECT(buffer), buffer_info_quark(), info, buffer_info_free);
    }
    return info;
}

static void hc_view_buffers_changed(WPEView *wpe_view, WPEBuffer **buffers, guint n_buffers)
{
    HcView *view = HC_VIEW(wpe_view);
    guint32 ids[16];
    guint n = MIN(n_buffers, G_N_ELEMENTS(ids));
    for (guint i = 0; i < n; i++)
        ids[i] = buffer_info(view, buffers[i])->id;
    if (view->sink && view->sink->ring)
        view->sink->ring(view, ids, n, view->sink_data);
}

static gboolean hc_view_render_buffer(WPEView *wpe_view, WPEBuffer *buffer, const WPERectangle *damage,
                                       guint n_damage, GError **error)
{
    (void)damage;
    HcView *view = HC_VIEW(wpe_view);
    if (!WPE_IS_BUFFER_DMA_BUF(buffer)) {
        g_set_error_literal(error, WPE_VIEW_ERROR, WPE_VIEW_ERROR_RENDER_FAILED, "not a dma-buf buffer");
        return FALSE;
    }
    if (view->pending) // should not happen: WebKit waits for frame done; complete the old one
        hc_view_frame_done(view);

    BufferInfo *info = buffer_info(view, buffer);
    if (!g_hash_table_contains(view->held, GUINT_TO_POINTER(info->id)))
        g_hash_table_insert(view->held, GUINT_TO_POINTER(info->id), g_object_ref(buffer));
    view->pending = g_object_ref(buffer);

    WPEBufferDMABuf *dmabuf = WPE_BUFFER_DMA_BUF(buffer);
    HcFrame frame = {
        .id = info->id,
        .first_time = !info->announced,
        .width = wpe_buffer_get_width(buffer),
        .height = wpe_buffer_get_height(buffer),
        .fourcc = wpe_buffer_dma_buf_get_format(dmabuf),
        .modifier = wpe_buffer_dma_buf_get_modifier(dmabuf),
        .n_planes = MIN(wpe_buffer_dma_buf_get_n_planes(dmabuf), HC_MAX_PLANES),
        .n_damage = n_damage,
        .buffer = buffer,
    };
    for (guint32 i = 0; i < frame.n_planes; i++) {
        frame.fds[i] = wpe_buffer_dma_buf_get_fd(dmabuf, i);
        frame.offsets[i] = wpe_buffer_dma_buf_get_offset(dmabuf, i);
        frame.strides[i] = wpe_buffer_dma_buf_get_stride(dmabuf, i);
    }
    struct stat st;
    if (frame.n_planes && fstat(frame.fds[0], &st) == 0)
        frame.inode = st.st_ino;
    info->announced = TRUE;

    if (view->sink && view->sink->frame)
        view->sink->frame(view, &frame, view->sink_data);
    return TRUE;
}

void hc_view_frame_done(HcView *view)
{
    if (!view->pending)
        return;
    WPEBuffer *buffer = view->pending;
    view->pending = NULL;
    wpe_view_buffer_rendered(WPE_VIEW(view), buffer);
    g_object_unref(buffer);
}

void hc_view_release(HcView *view, guint32 id)
{
    WPEBuffer *buffer = NULL;
    if (!g_hash_table_steal_extended(view->held, GUINT_TO_POINTER(id), NULL, (gpointer *)&buffer))
        return;
    wpe_view_buffer_released(WPE_VIEW(view), buffer);
    g_object_unref(buffer);
}

WPEBuffer *hc_view_lookup(HcView *view, guint32 id)
{
    return g_hash_table_lookup(view->held, GUINT_TO_POINTER(id));
}

gboolean hc_view_has_pending_frame(HcView *view)
{
    return view->pending != NULL;
}

void hc_view_set_sink(HcView *view, const HcFrameSink *sink, gpointer user_data)
{
    view->sink = sink;
    view->sink_data = user_data;
}

static void on_toplevel_changed(WPEView *view, GParamSpec *pspec, gpointer user_data)
{
    (void)pspec;
    (void)user_data;
    WPEToplevel *toplevel = wpe_view_get_toplevel(view);
    if (!toplevel) {
        wpe_view_unmap(view);
        return;
    }
    int width, height;
    wpe_toplevel_get_size(toplevel, &width, &height);
    if (width && height)
        wpe_view_resized(view, width, height);
    wpe_view_map(view);
}

static void hc_view_constructed(GObject *object)
{
    G_OBJECT_CLASS(hc_view_parent_class)->constructed(object);
    g_signal_connect(object, "notify::toplevel", G_CALLBACK(on_toplevel_changed), NULL);
}

static void hc_view_dispose(GObject *object)
{
    HcView *view = HC_VIEW(object);
    view->sink = NULL;
    g_clear_object(&view->pending);
    g_clear_pointer(&view->held, g_hash_table_unref);
    G_OBJECT_CLASS(hc_view_parent_class)->dispose(object);
}

static void hc_view_class_init(HcViewClass *klass)
{
    GObjectClass *object_class = G_OBJECT_CLASS(klass);
    object_class->constructed = hc_view_constructed;
    object_class->dispose = hc_view_dispose;
    WPEViewClass *view_class = WPE_VIEW_CLASS(klass);
    view_class->render_buffer = hc_view_render_buffer;
    view_class->buffers_changed = hc_view_buffers_changed;
}

static void hc_view_init(HcView *view)
{
    view->held = g_hash_table_new_full(g_direct_hash, g_direct_equal, NULL, g_object_unref);
}
