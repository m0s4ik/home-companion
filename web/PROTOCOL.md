<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# home-companion-web socket protocol (version 1)

The body (`home-companion`) and the web view helper (`home-companion-web`) talk over one Unix socket of type
`SOCK_SEQPACKET` (`home-companion-web.sock`): the helper listens, the body connects. One message per packet;
the dma-buf planes of a frame travel as `SCM_RIGHTS`.

Implementations, kept in sync by hand:

- Rust: `crates/webview-proto` (encode/decode, send/recv with fds, tests).
- C: `web/home-companion-web/protocol.h` (structs, constants, inline `hcw_send` / `hcw_recv`, builders for
  the variable messages). `web/protocol-test.c` checks it against the same bytes as the Rust tests:
  `cc -std=c11 -Wall -Wextra -Werror -o /tmp/protocol-test web/protocol-test.c && /tmp/protocol-test`.
- Test tools: `crates/webview-fake` (a producer that stands in for the helper) and
  `crates/webview-viewer` (a KMS viewer that stands in for the body).

## Framing

Every message starts with an 8-byte header; all fields little-endian, naturally aligned, sizes
multiples of 4:

| Offset | Type | Field |
|---|---|---|
| 0 | u16 | type |
| 2 | u16 | flags (0) |
| 4 | u32 | length of the whole message, header included (must equal the packet size) |

Maximum message: 64 KiB. Times are `CLOCK_MONOTONIC` nanoseconds (u64). Strings are UTF-8 without
NUL, preceded by their length, zero-padded to 4 bytes.

**Compatibility.** A receiver skips message types it does not know (closing any fds that came with
them) and ignores bytes after the fields it knows, so a later version can add messages and append
fields without breaking an older peer. Both sides send `hello` first; an incompatible change bumps
the version.

## Messages

H = helper → body, B = body → helper. Size = whole message.

| Type | Name | Dir | Size | Payload |
|---|---|---|---|---|
| 1 | `hello` | both | 24 | u32 version, u32 width, u32 height, u32 scale × 1000 |
| 2 | `frame` | H | 88 (+ fds) | u32 buffer, u32 width, u32 height, u32 fourcc, u64 modifier, u32 n_planes, u32 flags, u64 seq, u64 time, 4 × {u32 offset, u32 stride} |
| 3 | `drop` | H | 16 | u32 buffer, u32 reserved |
| 4 | `release` | B | 16 | u32 buffer, u32 reserved |
| 5 | `presented` | B | 32 | u32 buffer, u32 flags, u64 seq, u64 time |
| 6 | `show` | B | 8 | |
| 7 | `hide` | B | 8 | |
| 8 | `touch` | B | 32 | u32 kind, i32 id, f32 x, f32 y, u64 time |
| 9 | `menu` | H | 36 + items | u32 menu, i32 selected, i32 x, y, width, height, u32 n_items, then per item {u32 flags, u32 len, label} |
| 10 | `menu-result` | B | 16 | u32 menu, i32 index (-1 = cancelled) |
| 11 | `load` | B | 12 + url | u32 len, url |
| 12 | `reload` | B | 8 | |
| 13 | `back` | B | 8 | |
| 14 | `state` | H | 24 + strings | u32 state (0 loading, 1 ready, 2 error), u32 progress × 1000, u32 title_len, u32 message_len, title, message |
| 15 | `confirm` | H | 20 + path | u32 id, u32 kind (0 heavy view), u32 path_len, path |
| 16 | `confirm-result` | B | 16 | u32 id, i32 accepted (1 open, 0 stay) |
| 17 | `guard` | B | 12 + paths | u32 n_paths, then per path {u32 len, path} |

### hello

Sent by both sides right after connecting. The body states the view size and scale it wants
(1280×800, 1.25 today); the helper answers with what it renders. Frames carry their own size, so a
frame may differ from `hello` during a resize.

### frame, release, presented, drop: buffer life

- The helper assigns **buffer ids** (u32, never reused on a connection). The first `frame` of an id
  on a connection has `flags & 1` (`FRAME_FDS`) and carries **one fd per plane** (the same dma-buf
  may appear once per plane); later frames of that id carry no fds. The helper closes its fds after
  `sendmsg`. The body imports once per id (`eglCreateImageKHR(EGL_LINUX_DMA_BUF_EXT)` →
  `GL_TEXTURE_2D` via `glEGLImageTargetTexture2DOES`, no copy) and keeps the texture until `drop`
  or disconnect.
- `modifier` = `DRM_FORMAT_MOD_INVALID` (0x00ffffffffffffff) means an implicit layout: the body
  passes **no** modifier attributes to EGL then. Otherwise it passes the modifier for every plane.
- `seq` is the helper's frame counter, `time` when the frame was ready.
- **`presented`: every `frame` gets exactly one**, normally when the body's own frame that showed
  it has been flipped (`time` = flip time). If a frame will never be shown (a newer frame replaced
  it before the body drew it, or its buffer was dropped first) it is answered at once with
  `flags & 1` (`PRESENTED_SKIPPED`). The helper paces rendering on `presented` (WPE:
  `wpe_view_buffer_rendered`), so it never waits for a frame the body has discarded.
- **`release`**: the body will not sample this buffer again; the helper may render into it. Sent
  when a flipped frame no longer shows the buffer (the buffer on screen stays held, so the body can
  redraw from it at any time), or at once for a frame replaced before it was drawn. A buffer is
  never released twice without being sent again in between, and never after its `drop`.
- **`drop`**: the buffer is gone from the helper's ring (resize, web process restart). The body
  destroys its image and texture, and sends no `release` for it. Dropping the buffer on screen is
  allowed: the body keeps showing what it already drew (it is in the body's own scanout buffer)
  until the next frame.
- On disconnect both sides forget all ids; the body re-imports everything after the next `hello`.
- Synchronisation is implicit (msm reservation objects): the helper only needs to have submitted
  its rendering (`glFlush`) before sending; the body's GPU reads wait for it.

### touch

`kind`: 0 down, 1 motion, 2 up, 3 cancel (all points, e.g. after lost evdev events or when the
body takes the touch back). `id` is stable from down to up. `x`, `y` in view pixels of the
helper's `hello` size (before the device scale), origin top left: the body maps the panel's raw
coordinates through its rotation (270° on the ThinkSmart View: view x = (1 − raw_y / 1279) × 1280,
view y = raw_x / 799 × 800). The helper keeps the table of active points.

### show / hide, menu, load / reload / back, state

`show` / `hide` set the view's visibility (WebKit stops its compositor and rAF while hidden). The
body sends `show` right after `hello` if the view is on screen.

`menu` is a `<select>` opened by the page; item flags: 1 enabled, 2 group label (not selectable).
The body draws the list natively and answers with `menu-result` (index into `items`, or -1).

`load` navigates (the URL comes from the body's config); `reload`, `back` as in a browser.
`state` drives the body's spinner and error card; `title` is the page title, `message` the error.

### guard, confirm, confirm-result: heavy views

Added in P2 (types a version-1 peer skips). `guard` (sent by the body after `hello`, replacing any
earlier list) gives path prefixes of Home Assistant views that are heavy for the device (WebGL,
live video, endless animations). When the page navigates to a path under one of them (Home
Assistant moves between views with `history.pushState`), the helper does not open it but sends
`confirm` with the path; the body asks the user ("This view is very demanding...") and answers
`confirm-result`; on 1 the helper opens it. A prefix matches the path itself and anything under it
(`/dashboard-3d` matches `/dashboard-3d/0`, not `/dashboard-3dx`). Only one `confirm` is open at a
time: a newer one replaces it. A hidden body answers 0 at once.

### hello: the device scale

The helper applies the body's `scale` (its own `--scale` until the body says otherwise): the view
is laid out again at the new scale, the pixel size stays.

## What the body accepts (measured)

- fourcc: `AR24` (ARGB8888), `XR24` (XRGB8888), `AB24`, `XB24`; alpha is ignored (drawn opaque).
- modifiers: whatever `eglQueryDmaBufModifiersEXT` lists without `external_only`. On the ThinkSmart
  View (Adreno 506, freedreno, Mesa 25.0.7 and 26.3.0-rc3) that is **only `DRM_FORMAT_MOD_LINEAR`
  (0)** for every RGB format, plus `DRM_FORMAT_MOD_INVALID` (implicit, in practice linear on the same
  driver). GBM on the render node allocates linear buffers (modifier 0, stride 5120 for 1280 px) with
  or without a modifier list. One plane per frame for these formats.
