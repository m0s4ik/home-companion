<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# home-companion: design

home-companion turns a small Linux device with a microphone, a speaker and a screen into a voice companion
with a face. The first device is the Lenovo ThinkSmart View running
[thinksmart-view-debian](https://github.com/m0s4ik/thinksmart-view-debian); the first character is Alfred.
The screen itself is specified in [ui.md](ui.md).

## 1. Principles

1. **Light first.** One main process, no stutter. The screen never freezes while audio runs and the other way round.
   It should leave most of the device's 2 GB free.
2. **Body, brain, character.** The device is the *body*. Understanding and answering is Home Assistant's: its voice
   pipeline and the AI the user set up there. The *character* (look, phrases, sounds, personality) is a package
   that can be swapped.
3. **Reflexes stay local.** Wake word, end of speech, "stop", a few fixed commands, **timers and alarms** work without
   the network and answer at once.
4. **Secrets stay out of the repository.** The device holds the Home Assistant tokens, readable only by the body's
   user; they never go into the repository, logs or command lines.
5. **Develop on a laptop.** The same program runs in a window on a laptop (the simulator, section 7). A real device
   is needed for audio and the final check.
6. **Never leave a black screen.** If the GPU path cannot start, the software renderer takes over.

Non-goals: a skill system, a general app platform, a general-purpose browser (the body only embeds a web view for
Home Assistant, section 7), running a language model on the device.

## 2. Overview

```
                 ┌──────────────────────────── body (on the device) ──────────────────────────────────────┐
 microphone ──▶  │ audio in (16 kHz) ─▶ (AGC) ─▶ wake word + stop words ─▶ end of speech ─▶ utterance       │
                 │                                                    │               └▶ reflexes (Vosk)   │
                 │                                                    ▼                     │ match: act   │
                 │                                              state machine ◀── no match: to Home ───────┼──▶ Home Assistant
 speaker   ◀──   │ audio out: single owner of the pcm, resamples and mixes cues, bell, answers, AirPlay    │   (Wyoming)
 screen    ◀──   │ UI (Slint: GPU or software renderer) ◀── character (poses, animations, phrases)         │
 touch, keys ──▶ │ input, accelerometer, light sensor                                                      │
                 │ timers and alarms (the only place that rings)                                           │
                 │ Home Assistant link: Wyoming satellite, WebSocket (weather, lists, setup), web view      │
                 └─────────────────────────────────────────────────────────────────────────────────────────┘
                   platform layer: ALSA + fbdev/KMS + evdev + sysfs + iio on the device; a window and cpal on a laptop

 home-companion-brain (its own process and user on the device): tools for Home Assistant's agent over MCP
```

### Audio

The device's sound card has hard limits (see thinksmart-view-debian's docs): one player at a time (no software
mixing in the system), **S16_LE/S24_LE only**, and an amplifier that goes to standby after a stream, after which
the next sound would lose about half a second. So (`crates/audio`, its README has the details and the numbers):

- **One owner of the speaker**: the audio crate opens the pcm once and mixes everything itself (cues, timer bell,
  answers, announcements, AirPlay). No `aplay` processes.
- **The pcm stays open**, writing silence when idle (about 0.5 % of a core), so no sound is ever cut.
- **Resampling**: microphone 16 kHz, answers and sounds at their own rate; everything is resampled to one output
  rate (crate `rubato`) before mixing.
- **Microphone processing**: optional WebRTC AGC2 (automatic gain), off by default. Echo cancellation and noise
  suppression cost too much CPU on the device and are not used.
- **Volume**: a percentage put on the card's amplifier control (on the ThinkSmart View 'Speaker Amp Master', 2 dB
  per 5 % step, 100 % = 0 dB, the ceiling), or on the software gain of the mix where there is no such control
  (`crates/home-companion/src/volume.rs`). Kept in `<state_dir>/settings.toml`.
- **Microphone switch**: in the control centre; off, the wake word is not listened for (Talk on the screen still
  works).

## 3. Home Assistant does the thinking

The body is a **Wyoming satellite** (section 8). After the wake word the utterance goes to Home Assistant's own voice
pipeline: the user's speech-to-text, conversation agent and voice. Reflex transcripts are never sent: a closed
grammar forces its output and is not a real transcript.

**The body is agnostic of the AI.** The conversation agent is an agent of whatever AI integration the user
installed in Home Assistant; there is no API key on the device. Only what a normal Home Assistant installation has
is used: Wyoming, its built-in MCP client, the WebSocket and REST APIs with the token from the QR link, and
`ai_task`. No add-ons, no MQTT, no custom integration.

**Setup after the QR link** (`crates/home-companion/src/ha/setup.rs`): with the user's consent and each step shown on
the screen, the body adds the Wyoming integration for itself, the MCP integration for the brain's tools, the
"Alfred" agent under the AI integration it finds, the "Alfred" pipeline (speech-to-text and voice copied from the
preferred pipeline, local intents off so the gates cannot be bypassed) on its satellite, and the end-of-speech
sensitivity. Nothing that exists is changed; unlinking removes what it added. Settings → Home Assistant shows one
row per check and repairs what drifted.

### home-companion-brain

A small Rust process on the device (`crates/brain`, its own user and systemd unit) that lends Home Assistant's agent
**Alfred's tools over MCP** (HTTP, `http://<device>:10701/mcp`): memory, reminders, timers and alarms on the device,
the shopping list, and the house **with safety gates** (locks, alarms, door, gate and garage covers never by voice;
other covers and valves only after a spoken yes the satellite heard).

- **Memory stays on the device**: SQLite with FTS5 (no embeddings) in the brain's state directory, plus a profile
  and notes as Markdown blocks. Extraction at the end of a conversation and a nightly tidy-up think through Home
  Assistant's `ai_task`; without one they are skipped. "Forget all" on the screen deletes what the brain knows and
  keeps the reminders.
- **Reminders**: a card on the screen first, then Home Assistant speaks it on the device if nobody touched it;
  never by voice in the quiet hours. Late ones say so; missed ones become one card.
- The body talks to the brain over a Unix control socket (`crates/home-companion/src/brain_link.rs`): what the
  satellite heard and what Home Assistant said (the yes for the gates, the exchanges for memory), and the reminder
  cards back.

**Cancelling**: a Home Assistant pipeline that already understood a command cannot be cancelled from the satellite;
only the spoken answer can be dropped. That is why reflexes are decided before the audio is sent (section 5).

## 4. Characters

```
characters/alfred/
  character.toml      name, languages, wake word as shown, colours, which pose for which state,
                      phrases per language, animations
  persona.<lang>.md   the personality given to the "Alfred" agent in Home Assistant
  poses/*.png         one picture per state (idle, listening, thinking, speaking, error, offline, muted, sleeping…)
  animations/*.png    sheets of frames for loading, wake-up, idle, the states and the night (section 7)
  sounds/*.wav        the body's cues (wake, done, bell, timer set…)
  grammar/<lang>.toml the reflex phrases for Vosk
  wakeword/, stopword/ the microWakeWord models
```

**Every character lives in this repository**, under `characters/<id>/`, and a new one comes as a pull request. The
format is versioned (`format = 1` in `character.toml`); [characters/README.md](../characters/README.md) is its
specification and `cargo test -p character` checks every character against it. Optional items have a fallback
(a missing pose falls back to another, a missing sound to a synthesized tone). Alfred is CC0.

**A character is data, not code.** The `.slint` screens are compiled into the body, so layout, fonts, icons and the
interface's own words (`locales/<lang>.toml`, English by default) are the same for every character, which gives
poses, colours, phrases and sounds at run time.

## 5. Local reflexes

- **The wake word** ("Okay Alfred") is a microWakeWord model run by TensorFlow Lite Micro inside the body's process
  (`crates/wakeword`, with ESPHome's front end and detection rules).
- **"Stop" is a wake word too, not a Vosk command**: dedicated microWakeWord models ("basta", Italian for "enough",
  "stop", and "stop_alfred"), listened for while the character thinks, speaks or rings. They stop the answer, the
  bell or an announcement, and are the OK of a due reminder.
- **Vosk with a closed grammar for the rest** (`crates/reflex`, `characters/alfred/grammar/`): the time, timers,
  alarms, the volume and the music, with numbers and durations in the grammar and a small parser on the word sequence.
- **A grammar forces everything into it**, so a reflex fires only if the whole result is one of the grammar's
  phrases, every word's confidence is above a threshold (`[reflexes] threshold`, 0.80) and the words **cover the
  speech** the body's own energy-based end-of-speech detector heard (Vosk drops out-of-grammar words around a phrase).
  The decision waits for that detector, not for Vosk.
- **Order and latency**: the utterance is buffered while Vosk decodes it as a stream. At end of speech: match → act
  locally (an answer on the screen and a cue); no match → the buffered audio goes to Home Assistant in one burst
  followed by `audio-stop`, so Home Assistant does not wait for its own end-of-speech detection.
- **Isolation**: Vosk runs in a **helper process** (`home-companion-reflex`) started by the body, so a crash in its
  C++ code cannot take the screen down; the body restarts it with a growing pause and never waits for it. About
  140 MB with a language model loaded.

### Timers and alarms are local

Timers and alarms always ring from the body, even with the Wi-Fi or Home Assistant down
(`crates/home-companion/src/timers.rs`, `alarms.rs`):

- Set by a **reflex** or **by touch** (Timers and alarms page): created and kept in the body.
- Set through **Home Assistant**: the body **mirrors** each timer from the Wyoming timer events and rings at the end
  by its own clock, whether or not `timer-finished` arrives; when it does, it is matched by id and not rung twice.
  Pausing or cancelling one of Home Assistant's timers on the screen is local only (Wyoming cannot tell Home
  Assistant).
- **Alarms** (hour, minute, days) belong to the body only and ring like a timer.
- Saved in `<state_dir>/timers.json` and `alarms.json`, so they survive a restart.
- Stopped by the stop words, by touching the screen, or by Home Assistant.

## 6. Language and native parts

Rust for everything on the device; Python for the tools (animation sheets, icons, sounds, trimming the speech
models, training the stop words).

- **Native libraries**: Vosk (`libvosk.so`, in the helper process only), TensorFlow Lite Micro (static, in the body),
  optionally WebRTC audio processing (AGC2). The body is one main binary plus helpers, not a single static file.
- **Measured on the ThinkSmart View**: the body uses about 120 MB, the reflexes about 140 MB and the preloaded Home
  Assistant view about 450 MB.

## 7. Screen, UI and the simulator

- **UI toolkit: Slint** (`.slint` markup, GPL-3.0). Screens are `.slint` files in the body
  (`crates/spike-ui/ui/`); Rust provides the data (time, transcript, timers, states) and the dynamic parts (the
  sound wave, the volume HUD). **The visual system is [ui.md](ui.md)**.
- **Rendering: GPU first, software as fallback.** On a device with a GPU supported by Mesa the body uses Slint's
  linuxkms backend with the femtovg renderer (OpenGL ES through EGL/GBM, page flipping on vsync, feature `gpu`);
  femtovg is patched so text and icons stay sharp on the rotated panel (`third_party/`). The system provides Mesa (on
  the ThinkSmart View a reduced freedreno-only Mesa from thinksmart-view-debian). If EGL/GBM cannot start, the body
  uses the software renderer on its own fbdev platform (feature `fbdev`): it renders into RAM and copies the dirty
  rows. The same software renderer draws the simulator and the headless screenshots.
- **Animation frames**: `tools/make-animation.py` makes one **sheet** per animation, an RGBA PNG with the frames
  stacked, frame 0 complete and every other frame holding only the pixels that changed, cut to the box where the
  figure moves. Alfred's frames change in 1-6 % of the pixels, so the sheets are small in git and in memory: the body
  decodes them once in a thread, keeps the changes plus one key frame, and hands Slint one new image of the frame's
  box per frame.
- **Automatic brightness**: the backlight follows the light sensor on a log curve, smoothed, with hysteresis and a
  1.5 s fade; on the ThinkSmart View the body reads the sensor's light part directly over I²C so the proximity IR
  LED stays off (packaging/README.md). **Night mode** (Settings → Screen, 23-07 by default) caps the levels and shows
  the bedside clock (ui.md).
- **Backlight and awake time**: the home screen is dim with the character still; a touch, a volume key, a movement
  of the accelerometer or the end of a conversation makes it bright for 60 s (`[screen] awake_seconds`), while the
  character plays its wake-up animation and then the idle one; every other screen is bright. The panel is never
  switched off through DRM (it does not come back on the ThinkSmart View). **Vol+ and Vol− together** outside the
  Home Assistant view put the screen to sleep: backlight 0 until the next touch, key, movement, wake word or ringing
  timer; the body keeps running and listening.
- **Suspend** (Settings → Suspend {name}, `crates/home-companion/src/suspend.rs`): after a question, the whole device
  sleeps (s2idle, under 0.5 W). Vol+ or Vol− wakes it (touch does not); if a timer is pending, an RTC alarm wakes it
  shortly before it rings. The body writes `/sys/power/state` and the RTC's `wakealarm` itself, as its own user
  (group write access given by the unit at start); `HOME_COMPANION_SUSPEND_DRY_RUN=1` only checks and logs.
- **Volume keys and bar**: Vol+/Vol− step the volume by 5 %, repeated in software while held (the kernel does not
  repeat these keys); a press counts on release or after 300 ms held, since it may be half of Vol+ with Vol−
  (`ui/keycombo.rs`). A HUD at the top right (next to the keys) for 2 s and a short cue at the new level unless the
  speaker is busy.
- **Platform layer**: display, audio in/out, input and device controls (backlight, rotation, accelerometer, light
  sensor). On the device: fbdev/KMS, ALSA, evdev, sysfs, iio. In the **simulator** (`cargo run -p home-companion --
  --simulator`): a window of the panel's size, mouse = touch, keys for Vol+/Vol−, the laptop's microphone and
  speaker through `cpal`, a real Home Assistant.
- **Screenshots**: `--state <name> --screenshot FILE.png` renders any screen with fixed data (the golden screenshots
  and the docs).
- **Home Assistant's dashboards** are Home Assistant's **own frontend in an embedded WPE WebKit view**, not redrawn
  natively. The view lives in a helper process (`home-companion-web`, `web/`, C) that renders off screen on the GPU
  and hands each frame to the body as a dma-buf over a Unix socket (`web/PROTOCOL.md`); the body imports it as a
  texture inside its own UI and forwards touch. So the body stays the only owner of the screen and can draw over the
  page (the conversation bar, the home button), WebKit's libraries and crashes stay out of the body, and the view is
  **started at boot and kept loaded**, so it opens at once. The helper logs in with the body's Home Assistant tokens.
- **The view is full screen, so it has three ways back home**, all handled by the body (they work when the page or
  the helper hangs): the round home button, the swipe in from the button's edge, and Vol+ with Vol−. The details are
  in ui.md section 7.

## 8. Home Assistant

- **Finding Home Assistant**: at the first start the body looks for `_home-assistant._tcp` over mDNS (avahi) and lists
  what it finds (`crates/home-companion/src/ha/discover.rs`); the installer's `--ha-url` or the configuration's
  `[home_assistant] url` win.
- **Linking with a QR code** (`crates/home-companion/src/ha/{auth,link}.rs`): the body shows a QR code; the phone opens
  Home Assistant's own authorisation page (`/auth/authorize`, the standard flow its app and integrations use), where
  the user is usually logged in already, and confirms. Home Assistant redirects the phone to a small page served by
  the body only while the code is shown (`[home_assistant] link_port`, 8124; `client_id` and `redirect_uri` on the
  body's own address); the body exchanges the one-time code at `/auth/token` for an access token and a refresh token,
  stores them in `<state_dir>/ha-auth.json` readable only by its user, and refreshes them before they expire. No key
  to create, copy or paste; the device appears among the user's sessions in Home Assistant's profile and is revoked
  there. A long-lived token file stays possible as a fallback. The same tokens serve the setup, the weather, the lists
  and the web view.
- **Wyoming satellite** (`crates/wyoming`, protocol version 1.11.0): answers `describe` with `info` declaring only the
  features implemented, handles ping/pong, audio streams, timer events and **announcements** (audio outside a
  pipeline, answered with `played`).
- **Only Home Assistant may connect**: the satellite accepts connections only from the configured addresses and the
  ones learned from Home Assistant after the QR link (`[satellite] allowed`); the installer's firewall rules open the
  body's ports to the local network only.
- **WebSocket** (`crates/home-companion/src/ha/ws.rs`, a minimal client over plain TCP, no TLS): the weather of the
  home screen (`[weather] entity`, shown only with real data), the shopping list (`[lists] shopping`) and the setup.

## 9. AirPlay

The body runs **UxPlay** as a child process (patched, `tools/airplay/`) and stays the only owner of the speaker
(`crates/home-companion/src/airplay.rs`):

- **Music**: UxPlay's audio comes out as raw PCM on a pipe into a mixer stream, ducked while the character listens,
  thinks, speaks or rings. Our patch adds the sender's name, its volume slider (which becomes the body's own volume),
  the title, artist, album and cover. Pause, next and previous go back to the sender over DACP (found through avahi).
- **Screen mirroring** (`ui/mirror_video.rs`): UxPlay decodes the video with the hardware decoder and hands each frame
  over GStreamer's `unixfdsink` as a dma-buf; the body imports it into Slint's GL context, converts the colours on the
  GPU and gives the frame back at once. The layout is in ui.md.
- Optional: the installer's `--no-airplay` leaves it out.

## 10. Running on the device

- **Not as root**: dedicated users per service (`home-companion`, `home-companion-brain`, `home-companion-web`) with
  only the groups they need, a udev rule for the backlight, real-time priority for the audio thread through
  systemd (`LimitRTPRIO`), `Restart=on-failure`.
- **Installation**: `packaging/install.sh` from the release bundle; it asks nothing, never lets apt replace the
  image's Mesa, downloads the speech models and keeps a replaced configuration as `*.prev`
  ([packaging/README.md](../packaging/README.md)).
- **Logs**: structured logs to the journal (`journalctl -u home-companion`); every conversation ends with one log line
  carrying its latencies.
- **Language**: on-screen text, phrases and the reflex grammar per language (English, the default, and Italian),
  chosen on the first start's screen or in Settings → Language. The body's own words are in `locales/<code>.toml`
  (checked by `cargo test -p locale`), the character's phrases in its `character.toml`.

## 11. Privacy

- Audio leaves the device **only after the wake word**, only to Home Assistant, and is never saved on the device.
- The screen always shows when the microphone is streaming; the microphone switch stops the wake word.
- Reflexes, timers and alarms never leave the device, except as timer state the satellite reports to Home Assistant.
- The Conversations page keeps the **text** of the exchanges on the device (`<state_dir>/conversations.json`,
  readable by the `home-companion` user only, the last 7 days or 50 exchanges, can be switched off and cleared); it
  never leaves the device and audio is never kept.
- The brain's memory stays on the device; the pieces relevant to a request go to Home Assistant's agent through its
  tools.

## 12. Testing

- **Unit tests** on the pure parts (the state machine in `core.rs`, cards, timers, reflexes, the brain).
- **End to end**: `crates/wyoming`'s fake Home Assistant plays scripted Wyoming conversations against the body
  (`cargo test -p home-companion --tests`, `crates/home-companion/tools/run-e2e.sh`).
- **Golden screenshots**: each screen rendered headless with fixed data and compared with a reference PNG
  (`crates/home-companion/fixtures/golden/`, `crates/home-companion/tools/shoot-golden.sh`, `crates/home-companion/tools/check-golden.py`).
- **Characters and locales**: `cargo test -p character` and `cargo test -p locale` check every package.
- On the device: audio, the wake word and the final check.
