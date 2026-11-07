<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# spike-audio

M0 spike (a): audio was the main risk. It proves on
the ThinkSmart View's sound card (playback `hw:0,0`, capture `hw:0,1`, S16_LE/S24_LE only, no dmix) that
**one process owns the speaker** and mixes everything itself, while it captures 16 kHz from the
microphones, and it counts what is needed to decide with numbers:

- keep the pcm **open** and write silence when idle, or **close** it between sounds;
- whether WebRTC **AGC2 + noise suppression + AEC3** on the capture fits the Cortex-A53 budget.

It is a measuring tool, not the product.

**It never touches mixer controls.** There is no mixer code in `src/` (checked with grep below), so it
cannot unmute the amplifier. Volume is set by a human with `amixer` before a run.

## What is inside

| File | What |
|---|---|
| `src/main.rs` | command line: `probe`, `capture`, `play`, `duplex`; the main thread schedules sounds, drains the capture queue, prints and writes `--stats` JSON |
| `src/backend.rs` | `Playback` / `Capture` traits; `alsa` backend (interleaved S16_LE, period from `--period-ms`, buffer = 4 periods, blocking `writei`/`readi`, xrun recovery with `snd_pcm_prepare` and counters) and `file` backend (WAV out at real-time pace or as fast as possible, WAV in or silence); `probe` (hw_params tests only) |
| `src/mixer.rs` | the single playback owner's mixer: `Answer` (WAV at its own rate, resampled while playing with rubato's polynomial `Async`, mono duplicated to every channel) and beeps (880 Hz, 200 ms, 10 ms fades) placed at an absolute output frame; i32 bus saturated to i16 |
| `src/run.rs` | playback thread and capture thread, each with a bounded channel to the main thread (the audio threads never wait on it); per-period measurements; echo-click detection by normalized cross-correlation |
| `src/apm.rs` | WebRTC processor shared by the two threads: far end = the mixed playback downsampled to the capture rate (`analyze_render_frame`), near end = mean of the two microphones (`process_capture_frame`), 10 ms frames |
| `src/measure.rs` | `getrusage` (process and per thread), peak RSS (`VmHWM`), percentiles, a pacer for backends that do not block, `SCHED_FIFO` request |
| `tools/make-fixtures.sh` | `fixtures/generated/answer-22050.wav` (Piper, Italian voice, two sentences; synthetic vowel if Piper is missing) and `echo-16000.wav` (that answer as a microphone would hear it: 16 kHz, 120 ms late, -6 dB, noise) |
| `tools/build-webrtc-aarch64.sh` | builds upstream webrtc-audio-processing 2.1 for aarch64 with a meson cross file (see WebRTC below) |
| `tools/build-aarch64.sh` | cross-compiles both device binaries and stages them, stripped, in `out/aarch64/` |
| `tools/check-mix.py` | checks a mix WAV: rate, format, and the start of each 880 Hz beep (quadrature matched filter, so speech underneath does not fool it), plus each beep's level (dBFS) and duration; on a microphone capture of `--gap-test` (runbook U1) level and duration tell a beep present, shortened or missing |

Generated files (`fixtures/generated/`, `third_party/`, `out/`) are git-ignored.

## Build

```sh
export PATH=$HOME/.cargo/bin:$PATH CARGO_BUILD_JOBS=2
cargo build -p spike-audio --release
cargo test -p spike-audio --release
PKG_CONFIG_SYSROOT_DIR=/ PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig PKG_CONFIG_ALLOW_CROSS=1 \
  cargo build -p spike-audio --release --target aarch64-unknown-linux-gnu
```

With WebRTC on the host (needs `meson` and `ninja`; meson downloads abseil once):

```sh
cargo build -p spike-audio --release --features webrtc
```

For the device, everything at once (plain binary, WebRTC binary and its library, stripped):

```sh
crates/spike-audio/tools/build-aarch64.sh        # -> crates/spike-audio/out/aarch64/
```

### WebRTC and aarch64: what happened

- Features: `webrtc` = webrtc-audio-processing 2.1 with `bundled` (the -sys crate builds the C++ with
  meson); `webrtc-system` = the same crate linked against a prebuilt `libwebrtc-audio-processing-2`
  found with pkg-config. Without either, `--webrtc` fails with a clear message.
- **x86_64 with `webrtc` (bundled): works** (2 min 40 s with 2 jobs).
- **aarch64 with `webrtc` (bundled): does not work.** The -sys crate's `build.rs` runs
  `meson setup` with no cross file, so the C++ is compiled for the host and the link fails:
  `aarch64-linux-gnu/bin/ld: ...libwebrtc_audio_processing_sys-....rlib(audio_audio_processing_statistics.cc.o):
  Relocations in generic ELF (EM: 62)` ... `error adding symbols: file in wrong format`.
- **Fallback A, used for the device binary: works.** `tools/build-webrtc-aarch64.sh` copies the same
  upstream 2.1 sources out of the -sys crate, writes a meson cross file (aarch64-linux-gnu-gcc/g++,
  `cpu = cortex-a53`, NEON on) and builds one shared library with abseil linked in statically
  (`--force-fallback-for=abseil-cpp -Dabseil-cpp:default_library=static`) into
  `third_party/wap-aarch64/`. The spike is then built with `--features webrtc-system`, pkg-config
  pointed at that prefix, and an rpath of `$ORIGIN`, so on the device `spike-audio-webrtc` loads
  `libwebrtc-audio-processing-2.so.1` from its own directory. Extra runtime dependencies: only
  `libstdc++6` (already on Debian). Fallback B (Debian's 1.3 + a C++ shim) was not needed.
- The -sys crate's wrapper needs the abseil headers that meson installs next to the library; the build
  passes them with `CXXFLAGS_aarch64_unknown_linux_gnu` and `BINDGEN_EXTRA_CLANG_ARGS_aarch64_unknown_linux_gnu`.
- The bundled build runs `ninja` without `-j`; to keep to 2 jobs here, put a `ninja` wrapper that adds
  `-j2` first in `PATH`.

## Run

Every subcommand prints a summary; `--stats file.json` writes all counters. The default pcm is
`null` everywhere, so a mistyped command never makes a sound.

```sh
B=target/release/spike-audio
A=crates/spike-audio/fixtures/generated/answer-22050.wav
crates/spike-audio/tools/make-fixtures.sh

$B probe --pcm null --direction playback                     # on the device: hw:0,0 / hw:0,1
$B play --pcm null --answer $A --beep-at 1.0 --beep-at 2.5 --idle keep-open --seconds 6 \
   --stats crates/spike-audio/out/play-null.json
$B play --backend file --out crates/spike-audio/out/mix.wav --no-realtime \
   --answer $A --beep-at 1.0 --beep-at 2.5 --seconds 6
python3 -I crates/spike-audio/tools/check-mix.py crates/spike-audio/out/mix.wav 1.0 2.5
$B play --pcm null --idle close --gap-test --stats crates/spike-audio/out/gap-null.json
$B duplex --pcm-in null --pcm-out null --rate 16000 --seconds 10 --answer $A --beep-at 2 \
   --stats crates/spike-audio/out/duplex-null.json
$B duplex --pcm-in null --pcm-out null --seconds 10 --answer $A --webrtc      # --features webrtc build
# echo cancellation without hardware: the "microphone" is a file holding the answer's echo
$B duplex --backend file --in-wav crates/spike-audio/fixtures/generated/echo-16000.wav \
   --play-out crates/spike-audio/out/duplex-play.wav --answer $A --seconds 8 --webrtc
```

Options worth knowing: `--period-ms` (20 by default; buffer = 4 periods), `--out-rate` (48000; choose it
with `probe`), `--idle keep-open|close`, `--lead-silence-ms` (silence after each reopen in close mode),
`--gap-test` (beeps at 1, 5, 9 s, no answer, at least 10 s), `--rt` (SCHED_FIFO 10 for both audio
threads, a warning when refused), `--echo-click` (duplex: a click at 1.0 s, then the time from handing it
to the pcm to finding it in the capture), `--capture-out` / `--out` (WAV of what was captured,
processed mono with `--webrtc`).

What the numbers mean:

- `underruns` / `overruns` / `recoveries`: EPIPE on write / read, and successful `snd_pcm_prepare`.
- `snd_pcm_delay`: sampled before every write once the buffer is full (4 periods after an open).
- With `--no-realtime` every sound and the stop are queued before the playback thread starts, so the
  mix is deterministic (sounds land on their absolute frames).
- `request-to-first-frame`: from the moment the main thread queued the sound to the moment the period
  holding its first frame was handed to ALSA (in close mode it includes the reopen); "at the DAC" adds
  the delay measured at that write and the offset inside the period.
- `reopen`: open + hw_params + sw_params + prepare, per sound, in close mode.
- CPU: `getrusage(RUSAGE_THREAD)` of each audio thread over the run, also split for the playback thread
  into periods with sounds and periods of pure silence (the "always open" cost); process CPU with
  `RUSAGE_SELF`; all as % of one core. RSS: peak (`VmHWM`).
- WebRTC: wall time per 10 ms frame of `process_capture_frame` (AEC3, NS, AGC2 run there) and of
  `analyze_render_frame`; RMS of the microphone signal before and after processing while the mixer is
  playing and while it is idle (their difference while playing is the echo attenuation, NS included).

## Results on the build machine (x86_64, i7-1165G7, 2026-11-03)

These say the code is correct, not how the A53 will do; real numbers come from the runbook. ALSA's
`null` plugin never blocks, so the alsa backend paces it with the wall clock like a card with a
4-period buffer would; xruns cannot happen with it, and `snd_pcm_delay` reads 0.

| Run | Result |
|---|---|
| `cargo test` | 8 tests green: beep at the requested frame, 22050 -> 48000 length within 2 frames, tone frequency and level kept, saturation, file backend round trip, click detection, percentiles |
| play, null, keep-open, answer + 2 beeps, 6 s | exit 0, underruns 0; process CPU 0.3 % of a core, playback thread 0.28 % with sounds, 0.14 % on silence; RSS 5.0 MB |
| play, file, `--no-realtime` | 48000 Hz S16 stereo; `check-mix.py` (matched filter, the answer plays underneath) finds the beeps at 1.000 s and 2.500 s (+0.0 ms); the unit test shows the first beep frame is exact |
| play, null, close, `--gap-test` | exit 0, three reopens: 0.5, 0.1, 0.1 ms (null plugin; the device's DSP is the real question) |
| duplex, null, 10 s | exit 0, underruns 0, overruns 0, dropped 0; process CPU 0.49 % of a core; RSS 4.9 MB |
| duplex, null, 10 s, `--webrtc` | exit 0; WebRTC capture p50 0.16 ms / p95 0.41 ms per 10 ms frame (about 2 % of a core), render p50 < 0.01 ms; process CPU 2.5 %; RSS 8.3 MB |
| duplex, file, simulated echo, `--webrtc` | mic RMS while playing: -21.3 dBFS raw, -44.2 dBFS processed (about 23 dB of echo removed, NS included); capture p50 0.23 ms / p95 1.0 ms per 10 ms frame |
| `--rt` as a normal user | refused with EPERM (`ulimit -r` = 0 here), printed as a warning, run continues |
| aarch64 binaries | build, and run under qemu-user with the null pcm and the file backend (timings under emulation mean nothing) |

## Checks before a commit

```sh
! grep -rn -iE 'snd_mixer|alsa::mixer|Mixer::|amixer' crates/spike-audio/src   # no mixer controls
shellcheck -S warning crates/spike-audio/tools/*.sh
```

(`mixer.rs` and the type `Mixer` are the software mixer; the grep looks for ALSA mixer-control use.)
