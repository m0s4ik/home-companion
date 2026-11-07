# home-companion-web

The body's web helper: Home Assistant's frontend in WPE WebKit 2.54, rendered **off screen** on the GPU, each
frame handed out as a dma-buf for the body (`home-companion`) to show on its "Casa" screen.

Status: P2. With `--socket PATH` the helper speaks the body's protocol (`web/PROTOCOL.md`, version 1); the body
(`home-companion`, "Casa" screen) connects to it, `web/test/helper.sh` starts it for tests on the device;
`crates/webview-viewer` still stands in for the body with `web/test/casa.sh`. While no body is
connected (or without `--socket`) a local consumer stands in for it: it presents each frame at once (at most 60
per second) and gives the previous buffer back.

## How it works

- `hc-platform.c`: a minimal WPEPlatform implementation (`HcDisplay`, `HcToplevel`, `HcView`). WPE's own
  headless platform receives the same dma-bufs but releases each one by itself on a 60 Hz timer, and its types are
  final; ours hands every buffer to a sink and lets the sink decide when the frame is done and when each buffer is
  released. The GPU is the DRM render node (`/dev/dri/renderD128`); the primary node is never opened (the body is
  DRM master of the display).
- Frame flow: WebKit calls the view's `render_buffer` with a `WPEBufferDMABuf` (after its rendering fence has
  signalled) → sink `frame` (id, size, fourcc, modifier, planes) → `hc_view_frame_done()` =
  `wpe_view_buffer_rendered()` (WebKit may render the next frame) → `hc_view_release(id)` =
  `wpe_view_buffer_released()` (WebKit may draw into that buffer again). Buffer ids are stable per `WPEBuffer` (one
  per buffer of WebKit's ring); a sink `drop` comes when WebKit destroys one (resize, hide, web process restart).
- Socket (`--socket`): `SOCK_SEQPACKET`, one body at a time (a new connection replaces the old one). On connect:
  `hello`, the current `state`, and the latest frame at once (possibly stale: the page renders a fresh one when
  shown). `frame` per rendered buffer, with the dma-buf fds the first time an id is sent on the connection (the fds
  are the `WPEBuffer`'s own: the kernel duplicates them into the message, the helper has nothing to close);
  `presented` (shown or skipped) → `hc_view_frame_done()`; `release` → `hc_view_release(id)`; `drop` when WebKit
  destroys a buffer the body has. `show`/`hide` → view visibility and focus; `touch` → `wpe_event_touch_new` in
  logical units (view pixels / scale), the helper keeps the active points for `cancel`; `load`/`reload`/`back`;
  `state` from the load events (loading with progress, error with the message, ready when the page has its
  content, see below) and the title; `<select>` → `show-option-menu` → `menu` (items, selection, the element's
  rectangle in view pixels), `menu-result` → `webkit_option_menu_activate_item()` or `close()`. When the body goes
  away every buffer it held goes back to WebKit except the latest frame's (kept for the next body), and the local
  consumer takes over.
- `main.c`: the web view (persistent network session, 2D canvas and WebGL on the GPU, developer extras off,
  autoplay denied), `FD_MESA_DEBUG=sysmem` set before WebKit starts (freedreno's GMEM loop, see the spike page),
  the login, the control on standard input.
- `snapshot.c`: `--png`/`png`: imports the frame's dma-buf with EGL (`EGL_LINUX_DMA_BUF_EXT`), binds it to a GLES
  texture and reads it back, the way the body will import it.
- Login: with `--token-stdin` the first line of standard input is the `hassTokens` JSON of the device's long-lived
  token (never on the command line). A user script in a separate script world, at document start and only on the
  page's origin, writes it to `localStorage` when the frontend has none (first start, or after a logout) and sets
  `selectedTheme`. The persistent data directory keeps it, together with the frontend's cached code and icons.
- Heavy views: a third script, in the page's own world, wraps `history.pushState`/`replaceState`; a navigation
  to a path under one of the body's `guard` prefixes is held and sent to the helper (`hcGuard` message handler),
  which asks the body (`confirm`); on `confirm-result` 1 it opens the path (`pushState` + Home Assistant's
  `location-changed` event). The prefixes are kept in a user script, so they survive reloads.
- Device scale: the body's `hello` scale replaces `--scale` (the view is configured again).
- "Ready": a second script in the same world tells the helper when a Lovelace view has its cards in the DOM (or
  another panel, e.g. the profile, has rendered); the next frame is logged as "dashboard drawn" and the body gets
  `state` ready.

## Build (on the device, or in an arm64 Debian 13 root)

Needs WPE WebKit 2.54 (`libwpewebkit-2.0-dev`, our backport for Debian 13) and `build-essential pkg-config
libgbm-dev libegl-dev libgles-dev libdrm-dev libpng-dev`.

```sh
make
```

## Run

```sh
printf '%s\n' "$HASS_TOKENS_JSON" | home-companion-web --token-stdin --url http://HA:8123/lovelace/0 \
    --data-dir ~/.local/share/home-companion-web --cache-dir ~/.cache/home-companion-web --socket /run/home-companion-web/home-companion-web.sock
```

Run it as an unprivileged user in groups `video` and `render`; WebKit's bubblewrap sandbox stays on (Mesa must
be under `/usr`, the sandbox does not expose `/opt`).

| Option | |
|---|---|
| `--url URL` | page to open (required) |
| `--data-dir DIR`, `--cache-dir DIR` | persistent profile (required) |
| `--scale S` | device scale factor, default 1.25 (at 1 the 1280×800 view gets the desktop layout) |
| `--width PX`, `--height PX` | view size in pixels, default 1280×800 (landscape) |
| `--render-node PATH` | default `/dev/dri/renderD128` |
| `--theme dark\|light\|auto` | Home Assistant theme, default dark (`auto`: leave it to the frontend) |
| `--token-stdin` | first line of standard input: the `hassTokens` JSON |
| `--socket PATH` | listen for the body on this socket (mode 0660); without it the local consumer only |
| `--hidden` | start hidden |
| `--memory-limit MB` | web process memory limit (WebKit's memory pressure settings) |
| `--png FILE`, `--png-delay S` | save one frame as PNG S seconds (default 3) after the dashboard is ready |

Standard input, one command per line: `show`, `hide` (WebKit stops rendering and throttles the page), `load URL`,
`reload`, `back`, `png FILE` (the latest frame), `tap X Y` (a touch at view pixels X, Y), `quit`. End of input
quits, except with `--socket` (the body is in charge then).

The log on standard output has the buffer formats, WebKit's buffer ring, frames per second, and the times of the
first frame, "dashboard ready" and "dashboard drawn" (seconds since start).
