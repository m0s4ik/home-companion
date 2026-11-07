# spike-ui — M0 spike (b): Slint on the framebuffer

Go/no-go on Slint for the body's screen (design sections 7 and 13). One 800x1280 portrait screen with the
things the product will draw all the time, instrumented so the device numbers of section 6 can be read off:

- a character pose (600x600 PNG, placeholder face; `--pose a.png,b.png` loads any PNGs), swapped every
  `--pose-swap-every` seconds (`--full-swap` also changes the whole background: a full 800x1280 redraw);
- a large clock (HH:MM:SS, 160 px), updated when the wall-clock second changes;
- a 64x64 box moving along a rounded path and pulsing its colour, one step per tick at `--fps` (16) — driven from
  Rust, not by Slint's `animate`, so frame counts and dirty regions are exact (`--slint-animate` compares);
- a volume bar 600x24 (0–85, like 'Speaker Amp Master') stepped every `--volume-step-every` seconds;
- a status line with fps, frame time, RSS and CPU, rewritten every second (`--no-status` keeps it constant, so a
  clock frame holds only the clock).

Everything is pre-rendered at build time (`EmbedResourcesKind::EmbedForSoftwareRenderer`): the glyphs of DejaVu
Sans at the three sizes used and nothing loaded at run time, because the device image has no fontconfig fonts.

## Backends

| `--backend` | What | Where |
|---|---|---|
| `winit` | Slint's desktop window (simulator), software renderer; key **S** saves a screenshot | x86_64 default |
| `linuxkms` | Slint's own KMS backend, software renderer on DRM dumb buffers of `/dev/dri/card0`; with `SLINT_BACKEND_LINUXFB=1` its legacy `/dev/fb0` path | device |
| `fbdev` | our minimal Slint platform (`src/platform_fb.rs`): `MinimalSoftwareWindow` with `RepaintBufferType::ReusedBuffer`, a BGRX `TargetPixel`, only the dirty region reaches `/dev/fb0` (mmap) | aarch64 default |
| `headless` | the same platform rendering into RAM only (screenshots, CI) | anywhere |

`fbdev` has two modes: `--render-into fb` renders straight into the mapped framebuffer (no copy, but blending
reads back from framebuffer memory, which may be uncached); `--render-into ram` renders into a RAM back buffer and
copies the rows of the dirty bounding box. It switches the console to graphics mode (`KDSETMODE`, skipped with
`--no-vt`) and restores it on exit, Ctrl-C and SIGTERM. SIGINT/SIGTERM/SIGHUP handlers are installed for every
backend: they only set a flag, which our loop and, for `winit`/`linuxkms`, the scene timer poll to leave the event
loop normally (`quit_event_loop`), so destructors run. Slint's own legacy fbdev path (`SLINT_BACKEND_LINUXFB=1`)
switches the VT to graphics too and restores it only in its destructor; `spike-ui --reset-console` puts `/dev/tty0`
back to text mode if a run left it in graphics mode. With a plain file instead of a device (`--fb file
--fb-geometry 800x1280x3200`) the same code writes into the file: that is how it is tested here.

## Build

```sh
export PATH=$HOME/.cargo/bin:$PATH CARGO_BUILD_JOBS=2
cargo build -p spike-ui --release                    # x86_64, simulator (default feature)
PKG_CONFIG_SYSROOT_DIR=/ PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig PKG_CONFIG_ALLOW_CROSS=1 \
  cargo build -p spike-ui --release --target aarch64-unknown-linux-gnu --no-default-features --features fbdev,linuxkms
cargo test -p spike-ui --release
```

| Target | Features | Backends in the binary | Runtime libraries |
|---|---|---|---|
| x86_64 | `simulator` (default) | winit, fbdev, headless | libxkbcommon-x11-0 + an X server (or Wayland) |
| aarch64 | `fbdev,linuxkms` (no `simulator`) | linuxkms, fbdev, headless | glibc only |
| aarch64 | `fbdev,linuxkms-input` | + touch and keys through libinput | libinput10, libxkbcommon0 (not in the base image) |

`fbdev` and `headless` are always compiled in (the feature `fbdev` adds no dependency; it exists to name the
build). `simulator` uses only Slint's software renderer: no femtovg, skia or OpenGL. `linuxkms` is built without
libseat (the device opens `/dev/dri/card0` directly).

## Run here

```sh
crates/spike-ui/tools/run-headless.sh     # headless + fake framebuffer (both modes) + every check
crates/spike-ui/tools/run-xvfb.sh         # simulator under Xvfb, screenshot after 3 s, key S via xdotool
target/release/spike-ui                   # simulator window on a desktop; S = screenshot
target/release/spike-ui --help
```

Screenshots: `--screenshot out/x.png` writes one at exit, from our back buffer (`fbdev`, `headless`) or from
`Window::take_snapshot` (`winit`, `linuxkms`); in the simulator key S writes `out/x-key1.png`, `-key2`, ... (next to
`--screenshot`, or `out/spike-ui-keyN.png`). PNGs are RGB8.

`--stats out/x.json` writes: per frame the class (`first`, `pose`, `volume`, `clock`, `box`, `idle`: the most
expensive change it contains), render ms, present ms, dirty bounding box, dirty px and number of rectangles; per
second RSS and CPU % of one core; per class p50/p95/max; total frames, peak RSS, startup-to-first-frame; and the
pacing on the wall clock: `wall_s`, `ticks_total`, `achieved_tick_hz` (should equal `--fps`), `achieved_fps` (frames
drawn per wall second), `late_ticks` (inter-tick gaps longer than 1.5/fps: the missed ticks) and `max_tick_gap_ms`.
Slint restarts a late repeated timer from the moment it fired and scene time counts ticks, so on a slow device the
animation simply runs slower: only these wall-clock numbers show it. `frames_over_budget` counts frames whose render
+ present alone took longer than 1/fps. The same summary is printed as a table. With `winit` and
`linuxkms` Slint renders internally, so only ticks, RSS and CPU are recorded there.

Tools: `tools/make-pose.py` (pure python3) regenerates the CC0 placeholder poses `fixtures/pose-{a,b}.png`;
`tools/make-font.sh` regenerates the DejaVu Sans subset; `tools/check-png.py` checks a screenshot (800x1280 RGB, not
uniform, about one box worth of pixels in the box band) and, with `--fb`, compares a framebuffer dump with it pixel
for pixel; `tools/make-fake-fb.sh` creates the fake framebuffer file; `tools/device-session.sh free|check|restore` is the device
side of the runbook (finds and stops whatever holds the screen and the sound card, refuses to measure while anything
still does, restarts exactly those units afterwards).

## The companion screen (`companion-sim`)

Beside the spike, the crate builds the first real screen: `ui/companion.slint`, landscape 1280x800, for any
character of [characters/](../../characters/README.md) (poses, colours and phrases come from the character; layout,
Inter fonts and MDI icons from the body). It is compiled to its own file by `build.rs`, so none of it reaches the
`spike-ui` binary measured on the device.

```sh
cargo run -p spike-ui --release --bin companion-sim                      # Alfred, last language chosen
cargo run -p spike-ui --release --bin companion-sim -- --lang it --state listening
cargo run -p spike-ui --release --bin companion-sim -- --character characters/<id>
```

Keys 1-5: home, listening, thinking, speaking, error; N hides or shows the sample notifications (with none, the
home shows the character's invitation, "Say "Okay Alfred""); the Settings button, or a double click (double tap on the
device) on the character's mouth, opens the settings panel on the right (`--settings-open` at start), where the
language is chosen: it is saved in `~/.config/home-companion/settings.toml` (`--settings FILE`) and used at the
next start unless `--lang` says otherwise. The body's words come from [locales/](../../locales/README.md) (English if
a language has no file). S saves a screenshot (`out/companion-keyN.png`, or next to
`--screenshot`); "Talk" and "Stop" can be clicked. `--screenshot FILE --seconds N` saves and quits. The window runs at
scale factor 1 (`SLINT_SCALE_FACTOR`): the glyphs are pre-rendered at the sizes used, so at a Retina scale of 2 Slint
would draw smaller ones. With the Slint extension for VS Code, `ui/companion.slint` previews live with Alfred's
colours and sample data, but without the pose (it comes from Rust). `tools/make-font-inter.sh` regenerates the Inter
subsets. Slint pre-renders only ASCII and the characters found in the `.slint` strings, so `companion.slint` lists
every character the character format allows (`glyphs`): keep it in step with `printable` in `crates/character`.

## Measured here

On the build machine (x86_64 container, 4 vCPU; **not** the device — only to prove the instrumentation and the
partial redraw; the A53 will be several times slower). Release build, 16 fps, fake framebuffer file for `fbdev`:

| Run | class | frames | p50 / p95 / max ms (render + present) | mean dirty px |
|---|---|---|---|---|
| fbdev `--render-into fb`, box, 10 s | box | 149 | 0.05 / 0.07 / 0.08 | 5 072 (old + new 64x64 box) |
| | clock (+ status line, merged with the box) | 11 | 1.22 / 1.29 / 1.29 | 338 000–406 000 (bbox 800x562) |
| | first (full screen) | 1 | 4.3 | 1 024 000 |
| fbdev `--render-into ram`, box, 10 s | box | 149 | 0.06 / 0.08 / 0.59 | 5 072 |
| fbdev fb, `--full-swap` every 1 s | pose (800x1280) | 6 | 4.2 / 5.5 / 5.5 | 1 024 000 |
| | volume | 6 | 0.14 / 0.16 / 0.16 | 26 696 |
| fbdev ram, `--full-swap` | pose (800x1280) | 6 | 4.4 / 5.8 / 5.8 (render 3.8) | 1 024 000 |
| headless, pose swap (600x600) | pose | 2 | 2.5–2.7 | 360 000 (+ box) |
| headless `--slint-animate` | idle (animation frames) | 58 in 5 s | 0.05 | 5 320 |
| headless `--no-box --no-status`, 5 s (run D) | clock | 5 | 0.52 / 0.91 / 0.91 | 160 000 (800x200, 1 rect) |
| headless `--no-box`, status line on | clock | 5 | 0.56 / 1.04 / 1.04 | 184 576 (2 rects) |

Late ticks (inter-tick gaps > 1.5/fps) 0 everywhere, achieved tick rate 16.07–16.1 Hz; frames over budget 0; peak RSS 18–22 MB (fbdev/headless), 31 MB (winit under Xvfb); startup to first frame
15–30 ms; CPU 0.5–1.2 % of one core here. Under `qemu-aarch64` (emulated, timings meaningless) the aarch64 binary
renders the same picture: 16 frames, first frame 28 ms render. Binary: x86_64 stripped 17 MB, aarch64 stripped
11.7 MB, linked only against glibc (`libc`, `libm`, `libgcc_s`). With `--slint-animate` the frame where Rust sets a
new waypoint has 0 dirty px (Slint only starts the animation); the animation frames are classed `idle`.

## Slint caveats found

- **Fonts**: with `EmbedForSoftwareRenderer` the compiler rasterises, for every `font-size` literal in the
  `.slint` files, ASCII plus every character of the string literals. A size computed at run time or a character
  outside that set (e.g. `°`, accented letters coming from Rust strings) is not drawn. All sizes in `ui/app.slint`
  are static and listed at its top (160, 28, 22 px); `SLINT_FONT_SIZES` at build time adds sizes. The bitmaps are big:
  ASCII at 160 px is most of the ~5 MB `.rodata`. `software-renderer-systemfonts` + fontconfig remains possible for
  the simulator only. The committed font is a Latin-1 subset of DejaVu Sans (32 KB instead of 760 KB, made by
  `tools/make-font.sh`) to stay inside the fixture budget; the license is next to it.
- **Features**: `default-features = false` drops everything, including the image decoders.
  `Image::load_from_path` needs `image-decoders`, which only the winit/linuxkms backends pull in; poses are decoded
  with the `png` crate instead, so the fbdev-only build still loads PNGs. `slint/backend-winit` enables both X11 and
  Wayland; at run time winit needs `libxkbcommon-x11.so` (panics without it). `backend-linuxkms` without `-libinput`
  and `-libseat` builds and links only glibc.
- **fontconfig**: even with embedded fonts, Slint's text stack (parley -> fontique) links `libfontconfig.so.1`,
  which the device does not have. `Cargo.toml` adds `fontique` with `fontconfig-dlopen` so the library is opened at
  run time if present (fontique then just sees no system fonts). Check with `readelf -d ... | grep NEEDED`.
- **Pixel format**: implementing `TargetPixel` for our own `#[repr(C)] struct Bgrx {b, g, r, x}` works in 1.18.1
  (`blend` + `from_rgb`), so the renderer writes the panel's format directly — no conversion pass. Slint's own
  linuxfb path also accepts XRGB8888/BGRX-like 32 bpp.
- **Partial redraw**: `RepaintBufferType::ReusedBuffer` + `SoftwareRenderer::render` returns the dirty region; a box
  frame touches ~5 000 px (old + new box), the pose swap exactly 600x600, the clock alone (`--no-status`) its
  800x200 text element (160 000 px, one rectangle).
- **Dirty regions are merged (at most 3 rectangles)**: Slint's `DirtyRegion` keeps at most `MAX_COUNT = 3`
  rectangles; a fourth change is merged into the rectangle it grows least, i.e. into a bounding box. Small changes
  far apart therefore become one big rectangle: here a clock second also rewrites the status line (768x32 at y 1230)
  while the box moves between the two, and the frame becomes one 800x562 bounding box of 338 000–406 000 px instead of the ~190 000 px the four
  changes (clock, status, old and new box) cover. Without the box the clock + status frame stays two rectangles
  (184 576 px); with `--no-status` it is the clock alone (160 000 px). For the product's layouts: keep things that change in the same frame close together (or accept
  the merged area), and do not put a ticking element far from the animated character.
- `Window::take_snapshot` on the software renderer re-renders the whole window and resets the partial-rendering
  cache (it switches to `NewBuffer` and back), so the next frame is a full redraw: take screenshots at exit only.
- The software renderer has no rendering notifier, so per-frame times inside `winit`/`linuxkms` are not observable
  from the application; only our own platform measures them.
- Binary size: the release profile keeps line tables (`debug = "line-tables-only"`), so the unstripped binary is
  ~110 MB; the loaded part is text ~9 MB + rodata ~5 MB. Strip before copying to the device.
