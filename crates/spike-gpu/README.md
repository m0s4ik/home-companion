<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# spike-gpu: animate the character at 16 fps cheaply

M0 settled the screen on Slint's software renderer with our own fbdev platform (device
results): a 600x600 pose change costs 21 ms and Slint's
linuxkms backend with the software renderer manages only 11.5 fps at 99.6 % CPU. Since then Mesa 25.0.7 freedreno
works on the device (kmscube: 60 fps vsync-locked, 4.8 % CPU, OpenGL ES 3.1, FD506, EGL 1.5 on GBM). The open
question (design section 7, "Animation frames"): **can Alfred's real animation play at 16 fps cheaply, and how?**

One binary, one scene, three ways to draw it. The scene: the 800x1280 portrait panel, background `#03050D` (the
frames' own background), the character animation in a box centred at the top (440x760 at `--char-scale 1`, as the
Python screen program shows it), a 160 px clock HH:MM:SS below it. One animation frame per tick at `--fps` (16), the
next pose every `--pose-every` seconds (8: two loops of a 4 s animation), the clock when the second changes.

| `--mode` | How | Built with |
|---|---|---|
| `gpu` | Slint `backend-linuxkms` + **femtovg** renderer: OpenGL ES 2 context through EGL on a GBM surface (XRGB8888, scanout), full redraw each frame, page flip on vsync. Frames as GL textures uploaded **once** at the first frame (`--textures gl`, default) or as Slint images re-uploaded at each change (`--textures upload`). | feature `gpu` |
| `soft-blit` | No GPU. Frames decoded, **pre-scaled** to the on-screen box and converted to the framebuffer's BGRX **once at load**; each tick `memcpy`s the box rows into `/dev/fb0` (mmap). Slint's software renderer (our fbdev platform from spike-ui, rendering into RAM) draws only the background and the clock. | always |
| `soft-slint` | Slint's software renderer draws the pre-scaled frames (RGB8 images) itself: what M0 measured for a pose change, now at 16 fps. | always |
| `gpu-window` | `gpu`'s code in a desktop window (winit + femtovg), only to check the GPU path on the build machine (Xvfb + llvmpipe). | feature `gpu-window` |

## The frames

`alfred-animations-v0.1.zip` (CC0, release v0.1 of thinksmart-view-ha-companion, 32.6 MB): 5 poses (`answer`,
`error`, `idle`, `listening`, `thinking`) x **64 frames** (`frame_000.png` .. `frame_063.png`), **440x760 RGB**,
16 fps, a 4 s seamless loop each, ~100 KB per PNG. `tools/fetch-frames.sh [ZIP]` extracts them into
`fixtures/generated/alfred-animations/` (git-ignored); without an argument it downloads the zip with `gh`.

Memory per pose (64 frames): RGBA texture 85.6 MB, RGB8 image 64.2 MB, BGRX pre-scaled at scale 1: 85.6 MB.
That is why the default loads two poses (`--poses idle,listening`); `--poses all` loads the five (428 MB of
textures). The product would not keep every frame resident (design section 7: cropped, compressed, decoded while
playing): these runs measure the drawing, the memory numbers show what "everything resident" costs.

## Build

```sh
export PATH=$HOME/.cargo/bin:$PATH CARGO_BUILD_JOBS=4
# device binary: all three modes
PKG_CONFIG_SYSROOT_DIR=/ PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig PKG_CONFIG_ALLOW_CROSS=1 \
  cargo build -p spike-gpu --release --target aarch64-unknown-linux-gnu --features gpu
mkdir -p crates/spike-gpu/out && aarch64-linux-gnu-strip -o crates/spike-gpu/out/spike-gpu-aarch64 target/aarch64-unknown-linux-gnu/release/spike-gpu
# build machine
cargo build -p spike-gpu --release                       # soft modes only
cargo build -p spike-gpu --release --features gpu-window # + femtovg in a window (Xvfb test)
cargo test -p spike-gpu --release
crates/spike-gpu/tools/run-local.sh                      # soft modes on a fake framebuffer + gpu-window under Xvfb
```

The aarch64 `gpu` binary (stripped 12 MB) needs only `libgbm.so.1` besides glibc (`readelf -d`: libgbm, libgcc_s,
libm, libc; the DRM ioctls are done by the `drm` crate itself); EGL is opened at run time (`libEGL.so.1`, glutin)
and the GLES functions come from `eglGetProcAddress`. No libinput, no libseat, no libxkbcommon: the device opens
`/dev/dri/card0` directly (as root, or a user in the `video` group) and there is no touch in this spike.
fontconfig is opened with dlopen as in spike-ui (absent on the device: fine, the font is embedded).

## Device runbook

No sound is played. The screen program (Python) holds `/dev/fb0` and must be stopped first: spike-ui's
`device-session.sh` finds whatever holds `/dev/fb0`, `/dev/dri/card0` or the sound card, stops those units
(and mutes the amplifier), refuses to go on while anything still holds them, and `restore` starts exactly those
units again. Every run ends by itself after `--seconds` (scene time; a slow start adds a few seconds).

**Copy** (build machine, in the `spike/gpu` checkout, after the build above and `tools/fetch-frames.sh`):

```sh
S=/tmp/home-companion-spike-gpu; rm -rf "$S"; mkdir -p "$S"
cp crates/spike-gpu/out/spike-gpu-aarch64 "$S/spike-gpu"
cp crates/spike-ui/tools/device-session.sh "$S/"
cp -r crates/spike-gpu/fixtures/generated/alfred-animations "$S/"   # 32 MB, 5 x 64 PNG
scp -r "$S" <device>:/tmp/
```

**On the device** (`ssh <device>`; about 4 minutes of runs):

```sh
cd /tmp/home-companion-spike-gpu && D=$PWD
chmod 755 spike-gpu device-session.sh
DS() { sudo env HOME_COMPANION_SPIKE_DIR="$D" ./device-session.sh "$@"; }
ldd ./spike-gpu | grep -E 'not found|gbm'          # expect libgbm.so.1 => found, nothing "not found"
ls /usr/lib/aarch64-linux-gnu/libEGL.so.1           # EGL is opened at run time
DS free                                             # stops the screen program (and the audio holders)
DS check                                            # must report that nothing holds the screen
uptime | tee out-load-before.txt                    # load before

mkdir -p out
# G1: GPU, textures uploaded once (the candidate). Watch the panel: tearing?
sudo ./spike-gpu --mode gpu --seconds 30 --stats out/g1-gpu.json --screenshot out/g1-gpu.png 2>&1 | tee out/g1-gpu.txt
# G2: GPU, Slint images, re-uploaded at each frame change
sudo ./spike-gpu --mode gpu --textures upload --seconds 30 --stats out/g2-upload.json 2>&1 | tee out/g2-upload.txt
# G3: GPU, the GPU scales the character to 572x988, all five poses resident, a pose switch every 4 s
sudo ./spike-gpu --mode gpu --char-scale 1.3 --poses all --pose-every 4 --seconds 30 \
    --stats out/g3-scaled-all.json --screenshot out/g3-scaled-all.png 2>&1 | tee out/g3-scaled-all.txt
# S1: no GPU, pre-scaled BGRX frames copied into /dev/fb0. Watch the panel: tearing?
sudo ./spike-gpu --mode soft-blit --seconds 30 --stats out/s1-blit.json --screenshot out/s1-blit.png 2>&1 | tee out/s1-blit.txt
# S2: no GPU, the same at 572x988 (frames scaled once at load)
sudo ./spike-gpu --mode soft-blit --char-scale 1.3 --seconds 30 --stats out/s2-blit-scaled.json 2>&1 | tee out/s2-blit-scaled.txt
# S3: Slint's software renderer draws the frames (compare with M0's 21 ms pose)
sudo ./spike-gpu --mode soft-slint --seconds 30 --stats out/s3-slint.json --screenshot out/s3-slint.png 2>&1 | tee out/s3-slint.txt

sudo ./spike-gpu --reset-console                    # only if the text console did not come back
DS restore                                          # restarts exactly what `free` stopped
tar czf spike-gpu-results.tar.gz out out-load-before.txt
```

Back on the build machine: `scp <device>:/tmp/home-companion-spike-gpu/spike-gpu-results.tar.gz .`. Expected in `out/`:
`g1-gpu.{json,png,txt}`, `g2-upload.{json,txt}`, `g3-scaled-all.{json,png,txt}`, `s1-blit.{json,png,txt}`,
`s2-blit-scaled.{json,txt}`, `s3-slint.{json,png,txt}`. The PNGs can be checked here with
`tools/check-shot.py out/g1-gpu.png --frames-dir fixtures/generated/alfred-animations --box 180,40,440,760`
(G3: `--poses answer,error,idle,listening,thinking --box 114,40,572,988 --max-diff 6`).

**By eye**: tearing across the character in G1 (page flips: should be none) and S1 (writes into the scanned-out
buffer: may tear); whether the text console comes back after each run (it should: the VT goes back to text mode
and the kernel restores its framebuffer console when the DRM device is closed).

If `--mode gpu` fails, the message says where: "refused to start" (backend selection), "could not create the
window" (DRM/GBM/EGL: rerun with `EGL_LOG_LEVEL=debug` and keep the output), "texture upload failed".
`sudo SLINT_DRM_OUTPUT=list ./spike-gpu --mode gpu` lists the connectors.

## Numbers to record

From each `.txt` (the table printed at exit) or `.json` (keys in brackets):

| Run | achieved fps [`achieved_fps`] | late ticks [`late_ticks`] | frame interval p50/p95/max ms [`frame_interval_ms`] | work per frame p50/p95 ms [`frame_total_ms`] | thread CPU per frame p50 ms [`frame_thread_cpu_ms`] | process CPU per frame p50 ms [`frame_process_cpu_ms`] | CPU % of one core [`cpu_pct_mean`] | GPU busy % [`gpu_busy_pct_mean`] | peak RSS MB [`peak_rss_mb`] | MemAvailable drop MB [`mem_available_drop_max_mb`] | first frame ms [`startup_to_first_frame_ms`] | load ms |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| G1 gpu, gl textures | | | | | | | | | | | | `upload.total_ms` |
| G2 gpu, upload | | | | | | | | | | | | `decode_ms` |
| G3 gpu, x1.3, 5 poses | | | | | | | | | | | | `upload.total_ms` |
| S1 soft-blit | | | | | | | | — | | | | `prepare_ms` |
| S2 soft-blit x1.3 | | | | | | | | — | | | | `prepare_ms` |
| S3 soft-slint | | | | | | | | — | | | | `prepare_ms` |

Also: `gl.renderer` and `gl.version` of G1 (expect FD506 / OpenGL ES), the `pose` class times (a pose switch),
`threads`, `drm_fdinfo_end` (the GPU memory the driver reports), tearing and console by eye.

What the keys mean:

- **work per frame**: gpu: femtovg building and submitting the GL commands (BeforeRendering to AfterRendering of
  Slint's rendering notifier); soft: Slint render into RAM + dirty-row copy + the character blit.
- **thread CPU per frame**: CPU time of the drawing thread over that work. **process CPU per frame**: CPU of the
  whole process (all threads, Mesa's included) from one frame to the next: the real per-frame cost, timer and
  swap/page-flip wait included. `cpu_pct_mean` is the same per second.
- **frame interval**: wall time between consecutive frames (62.5 ms at 16 fps); `late_ticks` counts scene-timer
  gaps over 1.5/fps, `achieved_tick_hz` should be 16.
- **GPU busy**: from `/proc/self/fdinfo` (`drm-engine-*`, if msm reports it); null in the soft modes.
- **MemAvailable drop**: system-wide, so it includes GPU buffers that the process RSS does not show.
- **first frame**: process start to the first frame shown, PNG reading and (gpu, `gl`) the texture upload included.
- **classes**: `first`, `pose` (a pose switch), `clock` (the second changed), `anim` (only the character),
  `idle` (a redraw with no scene change: expected 0 on linuxkms and fbdev; winit under Xvfb redraws once more
  after every frame). Pacing (`late_ticks`, `achieved_tick_hz`) and the per-second CPU samples start at the first
  frame, so loading and the texture upload are not counted as late ticks or as playing CPU.

Decision rule: the GPU path is worth it if G1 holds 16 fps with 0 late ticks well below the soft-blit cost
(S1) and stays tear-free; soft-blit is the fallback that needs no GPU stack at all.

## Measured here

Build machine (x86_64 container, 4 vCPU; **not** the device: these runs prove the code and the pictures, the A53
numbers come from the runbook). `tools/run-local.sh`, release build, 16 fps, 3.4 s, poses idle + listening, pose
switch every second, soft modes on a fake framebuffer file, `gpu-window` under Xvfb with Mesa 25.0.7 llvmpipe
(OpenGL ES 3.2, a software GL: its CPU numbers say nothing about the Adreno).

| Run | achieved fps | late ticks | frame interval p50 ms | work per frame p50 / p95 ms | process CPU per frame p50 ms | peak RSS MB | load ms |
|---|---|---|---|---|---|---|---|
| soft-blit 440x760 | 16.4 | 0 | 62.4 | 0.26 / 0.33 | 0.32 | 191 | 252 (decode + convert 128 frames) |
| soft-blit 572x988 | 16.4 | 0 | 62.4 | 0.39 / 1.36 | 0.45 | 305 | 947 (+ bilinear scaling) |
| soft-slint 440x760 | 16.4 | 0 | 62.0 | 1.85 / 2.29 | 1.91 | 149 | 217 |
| soft-slint 572x988 | 16.4 | 0 | 62.1 | 2.97 / 3.62 | 3.02 | 236 | 903 |
| gpu-window, `gl` textures | 32.5 (16 + 16 idle) | 0 | 17.2 | 0.83 / 1.00 | 21 (llvmpipe) | 312 | upload 271 (decode 189 + GL 82) |
| gpu-window, `upload` | 32.4 (16 + 16 idle) | 0 | 15.4 | 1.70 / 4.33 | 21 (llvmpipe) | 279 | 0 (decoded before) |

Every screenshot was checked by `tools/check-shot.py`: the character box matches the expected animation frame
(mean absolute difference 0.00 at 440x760, 0.02 at 572x988 against the frame scaled by Pillow), the rest is the
background, the clock is drawn, and in the soft modes the fake framebuffer is pixel-identical to the screenshot.
`gpu-window` thus exercises the whole femtovg path of `gpu` except DRM/GBM: backend and renderer selection, the
rendering notifier, the GL texture upload, borrowed textures, `take_snapshot`. The aarch64 binary was run under
`qemu-aarch64` (soft-blit on the fake framebuffer: same picture, check passed; `--mode gpu` without `/dev/dri`
fails cleanly with "could not create the window: Could not create an egl display").

## Risks known before the device run

- **DRM master**: `gpu` needs to be the DRM master of `/dev/dri/card0`. Nothing else may hold it (the Python
  program uses `/dev/fb0`, the kernel's fbdev emulation, which is not a master). Root or the `video` group.
- **No libinput/libseat**: built without both (no touch in this spike, no seat daemon on the device). Slint's
  linuxkms backend then opens the DRM device directly.
- **GBM format**: Slint asks GBM for an `XRGB8888` surface with `SCANOUT | RENDERING` and an EGL config whose
  native visual is XRGB8888 (alpha size 0) — the same format kmscube uses, so freedreno/msm should accept it.
  If EGL finds no config, that is the place to look.
- **CRTC/console restore**: Slint's linuxkms backend sets the CRTC at the first frame and never restores the
  previous one; at exit the process closes the device and the kernel puts its fbdev console back (drm "lastclose").
  We also switch the VT to graphics mode during the run and back to text at exit. `--reset-console` is the way out.
- **GLES 2 context**: femtovg asks for OpenGL ES 2.0 (Mesa gives a compatible newer context). NPOT textures are
  used with CLAMP_TO_EDGE and no mipmaps, which ES 2 allows.
- **Rendering notifier side effect**: with a notifier set, femtovg flushes the clear before BeforeRendering (one
  extra flush per frame); negligible, but it is not exactly what an application without a notifier does.
- **Upload timing**: G1's first frame includes decoding and uploading all frames (expect a few seconds for 128
  frames on the A53; `upload.decode_ms` vs `upload.upload_ms` split it).
- **Memory**: textures live in GEM buffers (system RAM shared with the GPU): not in RSS, visible in
  `mem_available_drop_max_mb` and `drm_fdinfo_end`. `--poses all` is 428 MB of textures.
