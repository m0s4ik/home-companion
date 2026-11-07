// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

#ifndef HC_SNAPSHOT_H
#define HC_SNAPSHOT_H

#include "hc-platform.h"

// Imports the frame's dma-buf with EGL on the display's render node and writes it to `path` as PNG.
gboolean hc_snapshot_png(WPEDisplay *display, const HcFrame *frame, const char *path, GError **error);

#endif
