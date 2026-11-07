// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

// A minimal off-screen WPEPlatform implementation for home-companion-web.
//
// WPE's own headless platform (WPEDisplayHeadless/WPEViewHeadless) does receive every frame as a
// WPEBufferDMABuf, but it releases each buffer by itself on a 60 Hz timer and its types are final, so the
// helper cannot hold a buffer while the body samples it. This file implements the three platform types
// (display, toplevel, view) with the GPU on a DRM render node and hands each rendered buffer to a sink;
// the sink decides when the frame is "done" (WebKit may render the next one) and when each buffer is
// released (WebKit may draw into it again).

#ifndef HC_PLATFORM_H
#define HC_PLATFORM_H

#include <wpe/wpe-platform.h>

G_BEGIN_DECLS

#define HC_TYPE_DISPLAY (hc_display_get_type())
G_DECLARE_FINAL_TYPE(HcDisplay, hc_display, HC, DISPLAY, WPEDisplay)

#define HC_TYPE_TOPLEVEL (hc_toplevel_get_type())
G_DECLARE_FINAL_TYPE(HcToplevel, hc_toplevel, HC, TOPLEVEL, WPEToplevel)

#define HC_TYPE_VIEW (hc_view_get_type())
G_DECLARE_FINAL_TYPE(HcView, hc_view, HC, VIEW, WPEView)

#define HC_MAX_PLANES 4

// One rendered frame. The fds are owned by the WPEBuffer (borrowed: dup them to keep them).
typedef struct {
    guint32 id;          // stable for the lifetime of the WPEBuffer (one per buffer of WebKit's ring)
    gboolean first_time; // this id is seen for the first time (a consumer imports it once)
    int width, height;   // in physical pixels
    guint32 fourcc;      // DRM fourcc
    guint64 modifier;    // DRM format modifier (DRM_FORMAT_MOD_INVALID if implicit)
    guint32 n_planes;
    int fds[HC_MAX_PLANES];
    guint32 offsets[HC_MAX_PLANES];
    guint32 strides[HC_MAX_PLANES];
    guint64 inode; // of plane 0's dma-buf
    guint n_damage;
    WPEBuffer *buffer; // borrowed
} HcFrame;

typedef struct {
    // A new frame. The view keeps the buffer until hc_view_release(id); WebKit renders nothing new
    // until hc_view_frame_done().
    void (*frame)(HcView *view, const HcFrame *frame, gpointer user_data);
    // A buffer id is gone for good (WebKit destroyed it: resize, web process restart).
    void (*drop)(HcView *view, guint32 id, gpointer user_data);
    // WebKit's ring of buffers changed (informational).
    void (*ring)(HcView *view, const guint32 *ids, guint n_ids, gpointer user_data);
} HcFrameSink;

// render_node: e.g. "/dev/dri/renderD128". primary_node is only a name WPEDRMDevice requires; it is never
// opened by this file.
WPEDisplay *hc_display_new(const char *render_node, const char *primary_node, GError **error);

// Sets the toplevel's logical size and scale; the views get the new size (buffers are size * scale).
void hc_toplevel_configure(HcToplevel *toplevel, int logical_width, int logical_height, double scale);

void hc_view_set_sink(HcView *view, const HcFrameSink *sink, gpointer user_data);
// The frame handed out last has been presented: WebKit may render the next one.
void hc_view_frame_done(HcView *view);
// The consumer no longer samples buffer `id`: WebKit may draw into it again.
void hc_view_release(HcView *view, guint32 id);
// The buffer with this id, if the view still holds it (borrowed), else NULL.
WPEBuffer *hc_view_lookup(HcView *view, guint32 id);
// Whether a frame is waiting for hc_view_frame_done().
gboolean hc_view_has_pending_frame(HcView *view);

G_END_DECLS

#endif
