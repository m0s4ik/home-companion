<!--
SPDX-License-Identifier: GPL-3.0-or-later
SPDX-FileCopyrightText: 2026 Andrea Mattera
-->

# wyoming

The Wyoming protocol as Home Assistant speaks it to a voice satellite: framing, typed events,
the satellite-side server, and a fake Home Assistant (`fake-ha`) that drives a satellite through
scripted scenarios. Blocking `std::net` sockets and threads, no async runtime.

| Module | What |
|---|---|
| `event` | framing: `read_event`, `write_event` (byte-compatible with `wyoming/event.py`) |
| `message` | `Message`: every typed event a satellite exchanges with Home Assistant |
| `info` | `Info::for_satellite`: the `info` answer to `describe` |
| `server` | `Server`, `Sender`: the satellite side (Home Assistant connects to us) |
| `net` | `Net`: allow-list entries (`192.0.2.10`, `192.0.2.0/24`, IPv6) |
| `client` | `Client`: a small synchronous client (tests, fake-ha) |
| `fake_ha` | `Scenario`, `Step`, `run`, `builtin`: the scripted Home Assistant |
| `stub` (hidden) | a minimal satellite built on `Server` (tests and the `stub-satellite` binary) |

## Protocol version

We write `"version": "1.11.0"` in every header: Home Assistant core pins `wyoming==1.11.0`. The wire
format is unchanged since 1.0; later versions only added (optional) fields. Sources read for this
crate (copies are not committed):

- `rhasspy/wyoming` tag `v1.11.0` (commit `0bf9f277b201257f7df70c5fb5834a8451dd48ba`):
  [`event.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/event.py),
  [`info.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/info.py),
  [`ping.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/ping.py),
  [`satellite.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/satellite.py),
  [`pipeline.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/pipeline.py),
  [`audio.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/audio.py),
  [`wake.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/wake.py),
  [`asr.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/asr.py),
  [`tts.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/tts.py),
  [`vad.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/vad.py),
  [`error.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/error.py),
  [`snd.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/snd.py),
  [`timer.py`](https://github.com/rhasspy/wyoming/blob/v1.11.0/wyoming/timer.py),
  [`CHANGELOG.md`](https://github.com/rhasspy/wyoming/blob/v1.11.0/CHANGELOG.md).
- `rhasspy/wyoming-satellite` tag `v1.4.1` (commit `ac0b4c7d14aa5471c1706c02361b49214272fe0f`,
  pins `wyoming==1.5.4`):
  [`wyoming_satellite/satellite.py`](https://github.com/rhasspy/wyoming-satellite/blob/v1.4.1/wyoming_satellite/satellite.py)
  (`SatelliteBase.event_from_server`, `_ping_server`, `_send_run_pipeline`, `WakeStreamingSatellite`),
  [`wyoming_satellite/__main__.py`](https://github.com/rhasspy/wyoming-satellite/blob/v1.4.1/wyoming_satellite/__main__.py)
  (the `Info` it declares).
- `home-assistant/core` branch `dev` at commit `586a41a00e4278531f5fdd38f20642941617d778`
  (2026-11-03), `homeassistant/components/wyoming/`:
  [`assist_satellite.py`](https://github.com/home-assistant/core/blob/586a41a00e4278531f5fdd38f20642941617d778/homeassistant/components/wyoming/assist_satellite.py)
  (`_run_pipeline_loop`, `_send_delayed_ping`, `_stream_tts`, `async_announce`, `_handle_timer`),
  [`data.py`](https://github.com/home-assistant/core/blob/586a41a00e4278531f5fdd38f20642941617d778/homeassistant/components/wyoming/data.py)
  (`load_wyoming_info`),
  [`const.py`](https://github.com/home-assistant/core/blob/586a41a00e4278531f5fdd38f20642941617d778/homeassistant/components/wyoming/const.py),
  [`__init__.py`](https://github.com/home-assistant/core/blob/586a41a00e4278531f5fdd38f20642941617d778/homeassistant/components/wyoming/__init__.py),
  [`manifest.json`](https://github.com/home-assistant/core/blob/586a41a00e4278531f5fdd38f20642941617d778/homeassistant/components/wyoming/manifest.json).

`fixtures/python-frames.bin` was written by the real wyoming 1.11.0 package
(`tools/make-python-frames.py`): the tests parse it and check that our header lines are
byte-identical to Python's (same keys, order, separators and lengths).

### Framing

One JSON line `{"type", "version", "data"?, "data_length"?, "payload_length"?}` + `\n`, then
`data_length` bytes of JSON merged into `data` (the block wins per key), then `payload_length`
bytes of payload. We write `data` as a separate block (never inline, nothing when empty) and
`payload_length` only for a non-empty payload, with Python's separators (`", "`, `": "`) and UTF-8
unescaped, as `async_write_event` does. Readers accept inline data, `data: null` and missing data.
Limits: header line 1 MiB, data/payload 16 MiB (`Error::Frame`).

### What we declare and handle

`info` (exact shape in `fixtures/info-expected.json`): `asr`, `tts`, `handle`, `intent`, `mic`,
`snd` empty; one `wake` program with the wake models (so Home Assistant resolves the `detection`
name to its phrase); `satellite` with `installed: true` (Home Assistant creates a satellite device
only then), `has_vad: false`, `active_wake_words`, `max_active_wake_words: 1`. Every artifact field
(`name`, `attribution`, `installed`, `description`, `version`) is always written, `null` when
unknown, because `DataClassJsonMixin.from_dict` does not default missing fields.

| Event | Direction | Keys written | Who handles it |
|---|---|---|---|
| `describe` -> `info` | HA -> sat -> HA | see above | server, at once |
| `ping` -> `pong` | both ways | `text` (null when none) | server answers HA's pings; pings HA 2 s after each pong, drops it after 5 s without pong |
| `run-satellite` / `pause-satellite` | HA -> sat | – | home-companion (online / offline) |
| `detection` | sat -> HA | `name`, `timestamp`, `speaker` (null when none) | home-companion |
| `run-pipeline` | sat -> HA | `start_stage`, `end_stage`, `restart_on_end`, `wake_word_name` (only when set) | home-companion (asr -> tts) |
| `audio-start` / `audio-chunk` / `audio-stop` | sat -> HA | `rate`, `width`, `channels`, `timestamp` (+ payload) | home-companion (mic, 16000/2/1) |
| `transcribe` | HA -> sat | `language` (only when set) | home-companion (logged) |
| `voice-started` / `voice-stopped` | HA -> sat | `timestamp` | home-companion |
| `transcript` | HA -> sat | `text`, `language` (only when set) | home-companion ("You said") |
| `synthesize` | HA -> sat | `text`, `voice` (`name`, `language`, `speaker`, only those set) | home-companion (answer text) |
| `audio-start` / `audio-chunk` / `audio-stop` | HA -> sat | as above; HA sends 22050/2/1 | home-companion (answer, announcement) |
| `played` | sat -> HA | – | home-companion, after the last sample left the card |
| `error` | HA -> sat | `text`, `code` (only when set) | home-companion (error screen) |
| `timer-started` | HA -> sat | `id`, `total_seconds`, `name`, `start_hours`, `start_minutes`, `start_seconds` (optional ones only when set) | home-companion (local timers) |
| `timer-updated` | HA -> sat | `id`, `is_active`, `total_seconds` (seconds **left**) | home-companion |
| `timer-cancelled` / `timer-finished` | HA -> sat | `id` | home-companion |
| `detect`, `not-detected`, `streaming-*`, `satellite-*` | – | as wyoming | typed, unused by a satellite |
| `transcript-chunk`, `synthesize-start/chunk/stop`, `select-program`, `handled`, `user-event`, ... | HA -> sat | – | `Message::Unknown`, logged at debug |

A known type with a missing required key (`transcript` without `text`, `timer-started` without
`id`) becomes `Message::Unknown` with a warning: a bad event never drops the connection. We do not
write wyoming 1.5.4's `name`/`snd_format` in `run-pipeline` (1.11.0's `RunPipeline.from_event`
does not read them).

## How Home Assistant behaves (things to know)

- **The 5 s rule.** `_run_pipeline_loop` waits at most `_PING_TIMEOUT = 5` s for *any* event from
  the satellite and sends `ping` `_PING_SEND_DELAY = 2` s after each `pong`. A satellite that does
  not answer is dropped and reconnected (`_RESTART_SECONDS = 3`); wyoming-satellite 1.0.0 suffered
  exactly this every ~8 s. Our server answers pings in the reader thread, so a busy core never
  causes a drop. Home Assistant also answers *our* pings (the `Ping.is_type` branch);
  wyoming-satellite 1.4.1 only starts pinging after the first server ping, we start 2 s after
  `run-satellite` (harmless), never while paused, and give up after 15 s without a `pong`.
- **Connection order.** Home Assistant is the TCP client. It connects, sends `run-satellite`, then
  `describe` (and reads the `info` again), then `ping` 2 s later. Besides that connection it opens
  short `describe -> info -> close` probes (`load_wyoming_info` in `data.py`: 5 s timeout, 3
  retries), from `WyomingService.create` in entry setup and in the config flow, where
  `async_step_zeroconf` calls it on every discovery *before* checking whether the entry exists:
  on a real install one arrives every ~30 s **while the satellite connection is up**. They must
  not disturb it: wyoming's `AsyncServer` runs one handler per connection, and wyoming-satellite's
  handler answers `describe` without claiming the satellite and only clears the server it
  claimed. Our server accepts concurrent connections the same way: only the one that sends
  `run-satellite` is the satellite link, probes are answered and otherwise ignored (covered by
  the `probe-while-connected` scenario).
- **Mute and reload.** Muting the satellite in Home Assistant sends `pause-satellite`; on unmute,
  reload or restart Home Assistant opens a new connection (the old one may linger until then).
  Home Assistant stops reading a paused satellite, so the server sends no `ping` while paused.
  A `run-satellite` on a new connection replaces the link (`Disconnect::Replaced`, reported
  before the new `Connected`).
- **Audio before synthesize.** When the pipeline sets `tts_start_streaming` in `INTENT_PROGRESS`,
  `_stream_tts` starts before the `TTS_START` event, so `audio-start` (and chunks) can arrive
  **before** `synthesize`. A satellite must start playing on `audio-start` and treat `synthesize`
  only as the text to show. Covered by the `tts-before-synthesize` scenario.
- **Answer audio.** `_stream_tts` sends `audio-start` with `timestamp: 0`, 2048-byte chunks
  (1024 samples) with ms timestamps, `audio-stop` with the final timestamp, at the WAV's format
  (Home Assistant asks TTS for 22050 Hz mono 16-bit). If `played` does not come it forces the
  entity back to idle after the audio length + ~2 s.
- **Announcements** (`async_announce`): `audio-start` with `timestamp: 0`, chunks, `audio-stop`
  **without** timestamp, outside any pipeline; it waits for `played` at most the audio length +
  0.5 s. Answer with `played` once the audio is out.
- **Microphone audio** is accepted only after `run-pipeline` (or a `detection`); Home Assistant
  converts it to 16 kHz/16-bit/mono itself but we send that format directly. `audio-start` from the
  satellite is ignored by Home Assistant (logged at debug) and harmless. An `audio-stop` from the
  satellite ends speech-to-text (the Stop button). wyoming-satellite stops streaming on
  `transcript`, `error` or `pause-satellite`.
- `transcript` from Home Assistant carries only `text` (no `language`); `transcribe` carries the
  pipeline language; `synthesize.voice` carries `name` and `language` when known; `error` codes
  seen: `stt-no-text-recognized`, `stt-stream-failed`, `intent-failed`, `tts-failed`.
- Timer events are forwarded only while connected: a timer must keep running and ring on the
  satellite even if the connection drops (it is mirrored locally; `timer-finished` from Home
  Assistant may arrive after the satellite already rang it).

## Server

```rust
let server = wyoming::Server::start(
    wyoming::ServerConfig {
        bind: "0.0.0.0:10700".parse()?,
        allowed: vec![wyoming::Net::parse("192.0.2.10")?],
        info: wyoming::Info::for_satellite(satellite_info),
        ..Default::default() // ping every 2 s, 16 MiB frames
    },
    Box::new(move |event| { let _ = core_tx.send(CoreEvent::Server(event)); }),
)?;
let sender = server.sender();             // clone it into the core
sender.send(&Message::Played)?;           // Err(NotConnected) = offline
sender.send_chunk(&AudioFormat::MIC, Some(ts_ms), &pcm_bytes)?;
```

Threads: `wyoming-accept` (blocking accept, allow-list), `wyoming-read` (one per live connection:
parses frames, answers `describe` and `ping`, forwards the rest to the sink), `wyoming-ping`
(100 ms ticker: our pings, the pong timeout, reaping finished readers). Writes go out under a
per-connection mutex with `SO_SNDTIMEO` 2 s; a failed or timed-out write closes the connection
(`Disconnect::Io("write timeout")`). `Server::shutdown` (or drop) returns within ~200 ms.

## fake-ha

```sh
cargo build -p wyoming --release
target/release/stub-satellite --bind 127.0.0.1:10701 &
target/release/fake-ha --connect 127.0.0.1:10701 --builtin what-time --out record.json
```

`fake-ha --connect HOST:PORT [--builtin NAME | --scenario FILE.toml] [--out FILE.json] [--no-ping]
[--verbose]`; `fake-ha --print NAME` prints a built-in scenario as TOML (a starting point for
`--scenario`; relative WAV paths resolve against the scenario file). Exit 0 = pass, 1 = a check
failed, 2 = usage or connection error. Built-ins: `what-time`, `tts-before-synthesize`,
`stop-button`, `timers`, `announce`, `probe-only`, `probe-while-connected` (see `src/fake_ha.rs`). With pings on (default)
it behaves like Home Assistant's loop, including the 5 s rule; `--no-ping` disables both. The
record (`--out`) holds the `info`, every received message (seconds since the scenario started,
type, data without payload), the seconds of microphone audio, `detection_to_first_chunk_ms`,
`audio_stop_to_played_ms` and the `failures`.

The fixtures `answer-22050.wav` (1.5 s, tones 660/880/1100 Hz) and `announce-22050.wav` (0.8 s)
are committed; `tools/make-fixtures.sh` regenerates them with sox.
