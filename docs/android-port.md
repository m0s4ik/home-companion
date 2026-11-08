<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Task: home-companion on an Android tablet (APK)

Give this file to a coding assistant as the task, after [AGENTS.md](../AGENTS.md). It is self-contained: do not ask
the maintainer what it already answers; open a question only for a choice listed under "Ask first".

## Goal

Someone without a ThinkSmart View can try the companion on an Android tablet: an APK that is the same body
(wake word on the device, the companion screen, reflexes, timers, Wyoming satellite for Home Assistant), built
from the same crates. The Linux device must not lose anything: same code, the platform parts behind
interfaces or features.

## Read first

- [AGENTS.md](../AGENTS.md), [design.md](design.md) (body and brain, lightness), [ui.md](ui.md) (the screen).
- `crates/home-companion/src/main.rs`, `core.rs` (the state machine, platform-free), `ui/mod.rs` (the `Screen` trait
  and renderers), `ui/renderer.rs`, `crates/audio/src/backend/` (ALSA and `cpal`), the features in
  `crates/home-companion/Cargo.toml` (`simulator`, `fbdev`, `gpu`): the simulator already runs the body on a desktop
  with `cpal` and a window, so most of the platform split exists.

## What changes, what stays

| Part | Linux device | Android |
|---|---|---|
| Screen | Slint on fbdev or KMS (`gpu`) | Slint's Android backend (`backend-android-activity-06`), same `.slint` files |
| Audio | ALSA (`audio/alsa`) | `cpal` (AAudio), the feature the simulator uses |
| Wake word and stop words | TensorFlow Lite Micro in the binary | same crate, built with the NDK (C++ via `cc`) |
| Reflexes | `home-companion-reflex` helper process with libvosk | Vosk's Android library; in-process or a thread, behind the same protocol |
| Home Assistant view | `home-companion-web` helper (WPE WebKit, dma-buf) | Android `WebView` in a second activity or view: no helper |
| Brain (`home-companion-brain`) | separate service: MCP over HTTP for Home Assistant, a Unix control socket for the body | a library in the same process, same MCP endpoint, direct calls or the same control protocol |
| AirPlay | UxPlay child process | off in the first version |
| Backlight, light sensor, suspend, volume keys | sysfs, evdev, iio, systemd | Android APIs, or off in the first version |
| Settings and state files | `/etc/home-companion`, `/var/lib/home-companion` | the app's private storage |

## Steps (each one ends green and committed)

1. **Build**: an `android` feature in `home-companion` and `audio`; the crate builds for `aarch64-linux-android` with
   `cargo-ndk`, and `cargo-apk`, `xbuild` or a small Gradle wrapper makes an APK. Linux builds unchanged
   (`cargo test -p home-companion --lib` and the device build still pass).
2. **Screen**: the companion screen shows on the tablet (landscape and portrait), touch works, the
   character animates. Screenshot in the PR.
3. **Audio and wake word**: microphone at 16 kHz mono and playback through `cpal`; "Okay Alfred" detected
   (log line `event=wake_word`); the microphone runs in a foreground service with its notification (Android
   stops background audio otherwise).
4. **Satellite**: the QR link and the setup in Home Assistant work from the tablet, Home Assistant reaches it over
   Wyoming (TCP), a question is answered with voice; timers and reflexes work with the Wi-Fi off.
5. **Home Assistant view** with `WebView`, the brain as a library, settings storage.

## Acceptance

- One codebase: no copy of `core.rs` or of the `.slint` files; Android-only code behind `cfg`/features.
- `cargo test` green on Linux; the ThinkSmart View build and behaviour unchanged (checked on the maintainer's
  device before merging).
- APK size and RAM measured and written in `docs/android.md`, with how to build and install it.

## Ask first

- The application id and name on Android.
- Publishing anywhere (app stores, GitHub releases).
