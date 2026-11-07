<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# audio

One owner of the speaker and the microphone for `home-companion` (design section 2, M1). It grew out of
[`spike-audio`](../spike-audio/README.md), whose numbers were measured on the ThinkSmart View in M0:
the pcm kept open costs 0.5 % of a core at 20 ms periods with 60 ms queued and no underruns, closing it
between sounds loses short sounds, and the full WebRTC chain (AEC3 + NS + AGC2) costs 28 % of a core,
a no-go. This crate keeps the pcm open, mixes everything itself and offers WebRTC **AGC2 only**, off
by default.

**It never touches mixer controls.** The volume is a software gain on the mix; there is no ALSA
mixer-control code in `src/` (`tests/features.rs` greps for it).

## What it does

`Audio::start(&AudioConfig, on_event)` opens the devices and runs two threads:

| Thread | Does |
|---|---|
| `audio-play` | every period (20 ms) mixes the active sources into an i32 bus (mono, duplicated to both channels), applies the volume, saturates to i16 and writes; silence when idle (`keep_open = true`, the M0 decision) or closes the pcm after `idle_close` (`keep_open = false`, logged reopen time). Optional SCHED_FIFO 10 (`realtime`; a refusal is logged). |
| `audio-capture` | reads periods of `capture_channels` interleaved 16 kHz samples, picks `Left` / `Right` / `Average`, re-chunks to exact 20 ms frames (whatever period the card rounded to), optional AGC2 on the two 10 ms halves, `Level` (RMS dBFS, peak), recorder tap, then an `Arc<[i16]>` frame to every subscriber with `try_send` (a full queue drops the frame and counts it: the audio thread never waits for a consumer). |

| API | |
|---|---|
| `Player::play(&clip)`, `play_repeated(&clip, times, gap)` | cues and the timer bell; `Clip::tones`, `Clip::from_wav` (any rate/channels, resampled once at load), `Clip::silence`; ready-made cues in `audio::cues` (`wake`, `done`, `bell`, `beep`) |
| `Player::open_stream(StreamFormat)` → `Stream` | an answer or announcement at any rate (8-48 kHz), width 1/2/4 bytes signed LE, 1-2 channels: `push` converts, downmixes and resamples (rubato polynomial cubic, as measured in M0) into the source's buffer on the caller's thread, so the mix lock is never held while resampling; playback starts once 100 ms are buffered or on `finish` |
| `Player::stop(id)` / `stop_all()` | 10 ms linear fade, then `AudioEvent::Stopped(id)` |
| `Player::play_at(&clip, at)` | *added*: places a clip at an absolute time of the output timeline (tests and benches with the file backend) |
| `Audio::subscribe(capacity)` | 20 ms 16 kHz mono `CaptureFrame`s |
| `Audio::recorder()` | the debug utterance recorder (below) |
| `Audio::stats()` | xruns, frames, drops, starved periods, CPU of each thread (`CLOCK_THREAD_CPUTIME_ID`), `queued_ms` (snd_pcm_delay), uptime |
| `Audio::output_rate()` | 48000, except with cpal on a device running at another rate |
| `audio::meter::capture_levels` | *added*: per-channel levels with only the capture open (`home-companion mic-levels`, `audio-bench levels`) |
| `audio::probe::alsa_formats`, `audio::wav` | the spike's `probe`; WAV helpers |

### `Finished` timing (what the integrator must know)

- `AudioEvent::Finished(id)` is sent when the source's last sample **has left the pcm**: the playback
  thread remembers the output frame just after the last sample and sends `Finished` once
  `frames written - snd_pcm_delay` passes it. With the pcm kept open the queue stays full of silence, so
  this is about 60 ms (3 periods) after the last sample was mixed on the device. This is when Wyoming
  `played` may go to Home Assistant.
- null backend and real-time file backend: the "delay" is the pacer's lead (up to 4 periods); non-real-time
  file backend: no delay, `Finished` right after the write of the last period.
- Exactly one terminal event per source: `Finished` or `Stopped`, never both. A `stop` on a source whose
  last sample was already mixed has no effect (its `Finished` follows). `Player::is_active(id)` is true
  until that event.
- `finish()` on a stream that never received data: `Finished` without audio, after one period.
- Dropping a `Stream` without `finish` or `abort` finishes it (whatever was pushed plays out, then
  `Finished`). `Stream::abort()` = `Player::stop(id)`.
- A started stream that runs dry before `finish` plays silence for that period and counts
  `stream_starved` in `Stats` (it resumes when data comes).
- Events come from the playback thread, through the callback given to `start`: keep it cheap (send on a
  channel). `Audio::stop()` blocks at most 500 ms; sources still playing get no event.

### File backend pacing (CI, the e2e test)

`Backend::File { capture, playback, realtime }`: capture from a 16 kHz WAV (mono or 2 ch, mapped to
`capture_channels`; silence after its end), playback to a 48 kHz stereo WAV (or discarded).

- `realtime = true`: both sides are paced by the wall clock like a card.
- `realtime = false` (deterministic):
  - the playback writes **as fast as possible while the mixer has something to play** (a clip, a scheduled
    `play_at`, a stream with data or finished) and paces the other periods like a card (a 4-period lead),
    so a long idle run does not produce gigabytes of silence and a 30 s e2e run lasts about 30 s;
  - a started answer stream that is momentarily empty makes the playback wait up to 50 ms for data
    instead of mixing a gap into it;
  - the capture **never runs ahead of the playback**: capture frame *k* is handed out only once the
    playback has written up to the same moment (the spike's `Pacer` idea, made explicit with a shared
    clock). A sound started in reaction to a captured frame (a wake cue after a detection) therefore lands
    a small, fixed distance after that frame in the output file;
  - the fan-out waits (up to 1 s) for room instead of dropping, and the first frame waits (up to 1 s) for
    a first subscriber, so a test that subscribes right after `start` sees every sample;
  - latencies measured on this path (detection → cue, audio-stop → `Finished`) bound the code, not a card.
- To place sounds on exact frames (`tests/mix.rs`, `audio-bench play --cues`), issue every `play_at`
  before anything that makes the mix race ahead.

### cpal (the simulator)

cpal calls back with buffers of whatever size the host likes, so the cpal backend makes them look like a
card to the same two threads:

- playback: the playback thread mixes 20 ms periods **at the device's rate and channel count** (the mixer
  runs at `Audio::output_rate()`; clips are resampled to it on `play`, streams resample straight to it) and
  writes them into a queue of at most 4 periods (`write` blocks like a full card buffer); the output
  callback drains it and plays silence, counted as an underrun, when it is empty. `queued_ms` = the queue.
- capture: the input callback only converts to f32 and `try_send`s the buffer on a bounded channel (a full
  channel counts as an overrun). The capture thread's `read` resamples each channel to 16 kHz with
  rubato's **windowed sinc** (128 taps, Blackman-Harris²) when the device is not at 16 kHz — an
  anti-aliasing low-pass before decimation, the M0 review note — then returns exact periods; the usual
  channel pick and re-chunking to 320 samples follow.
- `""`, `"default"` and ALSA names (`hw:...`) select the host's default devices; other names match the
  device's description or id.
- **Dependency note:** cpal 0.18 (the latest release) depends on alsa 0.11 / alsa-sys 0.4, which cannot
  share a workspace with alsa 0.12 / alsa-sys 0.6 (this crate and spike-audio; `links = "alsa"`), even as
  an optional dependency. cpal's master (0.19.0, unreleased) moved to alsa 0.12, so the crate pins it by
  git revision (`Cargo.toml`); switch to the crates.io release once 0.19 is published.

### Debug utterance recorder

Off by default (`AudioConfig.recorder = None`). Purpose: the user finds that Whisper often misunderstands
him; each file is exactly what the brain heard (16 kHz mono, after the channel pick and the AGC), one per
utterance, to build a speech-to-text bench from real requests. `Recorder::start(label)` opens
`<dir>/<yyyymmdd-hhmmss>-<label>.wav` (label sanitized, local time, `-2`… on a clash) and writes the last
`pre_roll_ms` kept in a ring while idle; the capture thread appends every frame through a buffered
writer; `stop()` finalizes it and deletes the oldest recordings beyond `keep`. The directory is created
with mode 0700 (voice is personal data). Idle, the recorder only fills its ring.

## Features

| Feature | |
|---|---|
| `alsa` (default) | the device backend; Linux only (elsewhere it does nothing) |
| `cpal` | the simulator backend (macOS, desktop Linux) |
| `webrtc` | AGC2 from the sources bundled in the -sys crate (meson + ninja); host only |
| `webrtc-system` | AGC2 against a prebuilt `libwebrtc-audio-processing-2` (pkg-config): `tools/build-webrtc-aarch64.sh` for the device |

`agc = true` without a webrtc feature: `Audio::start` returns `Error::Feature("webrtc")`; a backend whose
feature is missing: `Error::Feature("alsa" | "cpal")`.

## Build and test

```sh
export PATH=$HOME/.cargo/bin:$PATH CARGO_BUILD_JOBS=2
cargo build -p audio --release --all-targets --features cpal
cargo test -p audio --release                      # never opens a sound device (null and file backends)
cargo test -p audio --release --features webrtc    # + the AGC test (bundled WebRTC; ninja without -j: see spike-audio)
cargo clippy -p audio --all-targets --features cpal -- -D warnings
PKG_CONFIG_SYSROOT_DIR=/ PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig PKG_CONFIG_ALLOW_CROSS=1 \
  cargo build -p audio --release --target aarch64-unknown-linux-gnu --bin audio-bench
crates/audio/tools/build-aarch64.sh   # device binaries, with and without WebRTC, stripped, in crates/audio/out/aarch64/
```

The aarch64 WebRTC build needs, besides pkg-config pointed at `third_party/wap-aarch64/lib/pkgconfig`, the
abseil headers for the -sys crate's wrapper (`CXXFLAGS_aarch64_unknown_linux_gnu` and
`BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu` = `-I<prefix>/include`) and an rpath of `$ORIGIN`;
`tools/build-aarch64.sh` sets all of them (`tools/build-webrtc-aarch64.sh` prints the full command).

Tests (`tests/`): mixer (saturation, cues found at 1.000 s and 2.500 s ±1 ms at -10.46 dBFS over an answer
stream with a matched filter, the spike's `check-mix.py` in Rust; `play_repeated(3, 1 s)` exactly 1.2 s
apart; `stop` fades within 10 ms with no step above 0.1 full scale), streams (22050 Hz mono in 2048-byte
chunks to `round(n · 48000/22050)` ±1 frame; a 1 kHz tone keeps 1000 crossings/s and its level within
0.1 dB; 44.1 kHz stereo, 8 kHz 8-bit and 32-bit inputs; `finish` without data; exactly one terminal event),
capture (left/right/average, -6 dB average, a -20 dBFS tone within 0.5 dB, 320-sample frames with periods
of 50/30/7 ms, drops counted for a slow subscriber), recorder (pre-roll, contiguous content, rotation,
mode 0700, idle writes nothing), null duplex (paced, 0 xruns, `Finished`, stop < 500 ms), configuration
errors, no mixer-control code, SPDX headers. `AUDIO_MIX_WAV=mix.wav cargo test -p audio --test mix` also
checks a mix written by `audio-bench`.

Fixtures (`fixtures/`, made by `tools/make-fixtures.sh` with `sox -R`, 226 KB): `answer-22050.wav` (1.5 s,
440 then 554 Hz), `speech-16k.wav` (3 s, five bursts of 300-3400 Hz noise at -20 dBFS RMS),
`stereo-left-16k.wav` (1 s, 1 kHz at -6 dBFS on the left only).

## audio-bench

```sh
B=target/release/audio-bench
$B probe --device hw:0,0 --capture-device hw:0,1          # device: what the pcms accept, no sound
$B play --backend file --playback-file mix.wav --no-realtime --cues --seconds 6
python3 -I crates/spike-audio/tools/check-mix.py mix.wav 1.0 2.5    # or AUDIO_MIX_WAV=mix.wav cargo test --test mix
$B duplex --backend null --seconds 3
$B duplex --backend file --capture-file crates/audio/fixtures/speech-16k.wav --capture-channels 1 --no-realtime --agc
$B levels --backend alsa --seconds 30                      # device: left/right/average every second
$B play --backend alsa --seconds 60                        # device: keep-open idle cost (silence only)
```

Every run ends with `RESULT {...}`: the `Stats`, process CPU (% of a core), capture and playback thread
CPU per second of audio, peak RSS. The default backend is `null`, so a mistyped command makes no sound.

## Results on the build machine (x86_64, i7-1165G7, 2026-11-03)

| Run | Result |
|---|---|
| `cargo test -p audio --release` | 48 tests green (21 unit + 27 integration), about 7 s (the capture tests are paced in real time) |
| `cargo test -p audio --release --features webrtc` | green, with the AGC test (a quiet synthetic vowel raised by more than 6 dB) |
| `play --backend file --no-realtime --cues --seconds 6` | 26 ms wall; `check-mix.py`: beeps at 1.000 s and 2.500 s (+0.0 ms), -10.5 dBFS, 200 ms: PASS; 7 `Finished` |
| `duplex --backend null --seconds 3` | 150 frames, 0 xruns, 0 dropped, `queued_ms` 80; process CPU 0.34 % of a core, RSS 3.3 MB |
| aarch64 (`tools/build-aarch64.sh`) | `audio-bench` 0.86 MB stripped, NEEDED libasound/libgcc_s/libm/libc; `audio-bench-webrtc` 0.92 MB + `libwebrtc-audio-processing-2.so.1` 1.2 MB, RUNPATH `$ORIGIN`; both run under qemu-user (null duplex, AGC on the speech fixture: same levels as x86_64, the cues mix passes `check-mix.py`) |

### AGC2 CPU (capture thread, CPU seconds per second of audio)

`audio-bench duplex --backend file --capture-file <f> --capture-channels 1 --no-realtime [--agc]`, three runs
each, release build with `--features webrtc`:

| Input | AGC off | AGC2 on | AGC2 cost |
|---|---|---|---|
| `fixtures/speech-16k.wav` (noise bursts, -20 dBFS) | 0.0013 | 0.0064 | **0.0051 s/s = 0.5 % of an x86 core** |
| synthetic vowel, -53 dBFS (as in `tests/features.rs`) | 0.0014 | 0.0064 | 0.0050 s/s; mean frame level -63.9 → -49.2 dBFS |

On the A53 expect several times more (M0: the full chain was 2 % of a core here and 28 % on the device, a
factor of about 14, which would put AGC2 alone near 7 %). The device figure. To measure it on the device:
`home-companion stats`, or `audio-bench-webrtc duplex --backend alsa --agc --seconds 60` vs without
`--agc`.

Note: AGC2's adaptive digital gain only adapts on what its voice detector takes for speech; on the noise
bursts of `speech-16k.wav` its gain moves little (mean frame level -45.5 → -39.7 dBFS), on the synthetic
vowel it reaches its 15 dB initial gain.

## Files

| File | |
|---|---|
| `src/lib.rs` | public types, `Audio` (threads, start/stop) |
| `src/backend/{mod,alsa,cpal,null,file}.rs` | `Playback` / `Capture` traits, `Pacer`, the four backends |
| `src/mixer.rs` | the software mix bus: sources, scheduling, fades, volume, `Finished` bookkeeping |
| `src/player.rs` | `Player`, `Clip`, `Stream`, the playback thread |
| `src/capture.rs` | the capture thread: pick, re-chunk, AGC, level, recorder tap, fan-out |
| `src/resample.rs` | the spike's `StreamResampler` (cubic or sinc), `resample_clip` |
| `src/apm.rs` | WebRTC AGC2 |
| `src/recorder.rs`, `src/meter.rs`, `src/cues.rs`, `src/stats.rs`, `src/wav.rs`, `src/probe.rs` | as named |
| `src/bin/audio_bench.rs` | the bench (hand-rolled arguments) |
| `tools/make-fixtures.sh`, `tools/build-webrtc-aarch64.sh` (+ `aarch64-cross.ini`), `tools/build-aarch64.sh` | fixtures, device builds |
