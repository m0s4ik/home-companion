# Working on home-companion

Read this first; it is written for coding assistants (Claude Code, Codex, Gemini CLI…) and for people. The
maintainer's time is the scarce resource: work from these files, do not ask what they already answer.

## What this is

A light voice companion "body" for small Linux devices with a microphone, a speaker and a screen (first
device: Lenovo ThinkSmart View with [thinksmart-view-debian](https://github.com/m0s4ik/thinksmart-view-debian);
first character: Alfred). The body hears its wake word on the device, shows an animated character, runs
timers and simple commands ("reflexes") by itself, and is a Wyoming satellite for Home Assistant. The
thinking is Home Assistant's: its pipeline (speech-to-text, conversation agent, voice) as the user set it up;
`home-companion-brain` lends that agent tools over MCP (memory, reminders, shopping list, the house with safety gates).
**Lightness first**: RAM, CPU and dependencies are measured on a 2 GB arm64 device.

How it is built and why: [docs/design.md](docs/design.md); the screen: [docs/ui.md](docs/ui.md). A ready task for a
contributor or a coding assistant: [docs/android-port.md](docs/android-port.md).

## Map

| Crate | What |
|---|---|
| `crates/home-companion` | the body: state machine (`core.rs`), Wyoming satellite, reflexes, timers, the screen (`ui/`: `screen.rs` controller, `cards.rs`, renderers fbdev/GPU/window/headless, `webview/` for the Home Assistant view) |
| `crates/spike-ui/ui/*.slint` | the screen layout (Slint), used by `home-companion` |
| `crates/audio` | one owner of speaker and microphone (ALSA on Linux, `cpal` elsewhere) |
| `crates/wakeword` | microWakeWord models on TensorFlow Lite Micro |
| `crates/reflex` | the closed Vosk grammar (`characters/*/grammar/<lang>.toml`) and the `home-companion-reflex` helper |
| `crates/wyoming` | Wyoming protocol, the satellite server and a fake Home Assistant for tests |
| `crates/brain` | `home-companion-brain`: MCP tools for Home Assistant's agent, memory (SQLite FTS5), reminders, gates |
| `crates/character`, `crates/locale` | character packages (`characters/`), the body's words (`locales/`) |
| `crates/webview-*` | the protocol and test tools of the Home Assistant view helper (`web/`, C) |
| `crates/spike-*` | M0 experiments, kept for measurements |

## Commands

```sh
PATH=$HOME/.cargo/bin:$PATH
cargo test -p home-companion --lib            # the body (127+ tests, ~1 s)
cargo test -p brain                # the brain
cargo test -p home-companion --tests          # against the fake Home Assistant
cargo run -p home-companion -- --simulator    # the screen in a window with the laptop's microphone (default features)
cargo run -p home-companion -- --renderer headless --state home --exit-after 3 --screenshot /tmp/home.png
```

Device build (arm64, from an amd64 Debian with the arm64 multiarch libraries): `cargo build --release --target aarch64-unknown-linux-gnu -p home-companion
--no-default-features --features fbdev,gpu`. `git checkout Cargo.lock` afterwards if a local patch changed it.

## Rules

- Code, comments, commit messages and docs in **English**; every new source file starts with
  `// SPDX-License-Identifier: GPL-3.0-or-later` and `// SPDX-FileCopyrightText: 2026 Andrea Mattera`
  (character assets: CC0). No new dependency without saying why and what it costs in size.
- **No private data** anywhere: no IP addresses of a real network, host or room names, tokens, Wi-Fi names.
  Examples use `ha.example`, `192.0.2.x`.
- Every change comes with tests where the logic is testable (pure functions are preferred for that reason);
  `cargo test` green before a commit. UI changes come with a headless screenshot.
- Home Assistant is used only through what a normal installation has (WebSocket/REST with the user's token,
  Wyoming, its built-in MCP client, `ai_task`): no add-ons, no MQTT, no second API key on the device.
- Never control locks, alarms, doors, gates or garage doors by voice; covers and valves only after a yes.
- Commits small and described (what and why). Do not change the design, the licenses or the public repos'
  history; open a question instead.
