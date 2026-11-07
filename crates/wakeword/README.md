<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# wakeword

microWakeWord v2 models on TensorFlow Lite Micro: a streaming detector with ESPHome's rules. It is the wake word
of `home-companion`, linked into its process (decided in M0: no separate wake word service). It grew out of the M0 spike
`crates/spike-native` (measured on the device: 3.2 % of one Cortex-A53 core, RSS 4-9 MB, detected through the
room, 0 false accepts in 60 s); the spike stays as it was.

```rust
let model = wakeword::Model::load(Path::new("characters/alfred/wakeword/okay_alfred.json"))?;
let mut det = wakeword::Detector::new(&model, &wakeword::DetectorConfig::default())?;
for frame in mic_frames {               // 16 kHz mono i16, any length (home-companion: 320 samples = 20 ms)
    if let Some(d) = det.feed(&frame)? {
        // d.name = "okay_alfred", d.probability, d.at_samples / d.at
    }
}
// after a conversation, before listening for the wake word again:
det.reset();
```

## Semantics

- **Front end and model** exactly as ESPHome's `micro_wake_word`: 30 ms windows every `feature_step_size` ms
  (10), 40 filterbank channels 125-7500 Hz, noise reduction, PCAN, log scale, quantized with
  `round(v * 256 / 666) - 128`; the model is invoked every `stride` slices (its input's second dimension: 2 for
  `okay_alfred`, 3 for `okay_nabu`) and outputs one uint8 probability.
- **Window rule** (ESPHome): a detection when the sum of the last `window` probabilities exceeds
  `round(cutoff * 255) * window`. The window starts as zeros and is cleared after a detection.
- **Cool-off** (`DetectorConfig::cooloff`, 1 s = ESPHome's 100 slices): the rule is not checked for that long at
  start, after a detection and after `reset()`; probabilities are still recorded meanwhile.
- **Refractory** (`DetectorConfig::refractory`, 2 s, as the old `wakeword.service --refractory-seconds 2`): a
  window detection less than this after the previous *reported* one is not reported. It is counted in samples
  fed (`at_samples`), not wall-clock time, so it is deterministic. It only guards against double firing within
  one continuous listening period: `reset()` clears it (the detector is not fed while streaming, so otherwise
  the first ~2 s of wake mode after a conversation would be deaf); the cool-off after `reset()` already protects
  against the tail of the answer.
- **`feed`** consumes every sample of any length and returns at most one detection: two inside one call are only
  possible with a cool-off shorter than the slice, and then the first is returned (both are counted in `stats`).
  Detection times do not depend on how the stream is cut (tested with 320, 1000 samples and one call).
- **`reset()`** reinitialises the front end (noise estimates, PCAN), the model's streaming state (TFLM
  `MicroInterpreter::Reset()`: variable tensors and resource variables), the partial input slice, the window and
  the cool-off. A reset detector behaves exactly like a new one (tested). **The integrator must call it after
  every conversation, before going back to wake mode**: during streaming the detector is not fed, so without a
  reset its state would join the audio from before and after the conversation, and the tail of the answer or of
  the user's speech could trigger it.
- **Arena**: `arena_bytes: None` starts from max(manifest's `tensor_arena_size`, 32 KiB) and grows by 1.5x while
  `AllocateTensors` fails, up to `arena_max` (1 MiB), then `Error::Arena`. `Some(n)` uses exactly n bytes (for
  measurements). Manifests lie: `okay_alfred` declares 30000 and needs 42912 (gets 49152); `okay_nabu` declares
  26080 and uses 22448. The resource-variable arena is 1024 bytes scaled by pointer size (2048 on 64-bit), as
  the spike found necessary.
- **CPU**: `Stats::cpu_s` is the calling thread's CPU time (`CLOCK_THREAD_CPUTIME_ID`) spent in `feed`.
- TFLM's log messages do not reach stderr: the shim provides TFLM's `DebugLog` and keeps the text for the
  error returned by `Detector::new`.
- `Detector` is `Send`, not `Sync`.

## Build

```sh
export PATH=$HOME/.cargo/bin:$PATH CARGO_BUILD_JOBS=2
crates/wakeword/tools/fetch-tflm.sh          # idempotent; or copy the spike's: cp -a crates/spike-native/third_party/{tflite-micro,tensorflow,deps} crates/wakeword/third_party/
cargo build -p wakeword --release
cargo test -p wakeword --release             # always --release: TFLM's reference kernels are slow in debug
PKG_CONFIG_SYSROOT_DIR=/ PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig PKG_CONFIG_ALLOW_CROSS=1 \
  cargo build -p wakeword --release --target aarch64-unknown-linux-gnu --bin wakeword-run
```

`build.rs` stops with a message naming `tools/fetch-tflm.sh` when `third_party/` is missing. The C/C++ build is
the spike's (pins, flags, archives and link order are documented in `crates/spike-native/README.md`), minus Vosk
and minus TFLM's `debug_log.cc` (replaced by the shim). About 150 C++ files, ~3 minutes per target with 2 jobs.
The binaries need only libstdc++, libgcc_s, libm and libc. Third-party notices: `THIRD_PARTY.md`.

### Why not `create_tflm_tree.py`

TFLM's `create_tflm_tree.py` runs the TFLM Makefile, which runs Python code from the downloaded tree. Executing
downloaded code is avoided: `tools/fetch-tflm.sh` only downloads (git, curl) and checks the two archives against
the md5 sums pinned by TFLM, and `build.rs` selects the sources the way the pinned Makefile does for a host build
(`MICROLITE_CC_BASE_SRCS` wildcards and the `MICROLITE_CC_KERNEL_SRCS` list from `tools/make/sources.inc`, minus
tests, benchmarks and test helpers).

## `wakeword-run`

```sh
wakeword-run --manifest M.json [FILE.wav...] [--positives FILE.wav...] [--negatives FILE.wav...] \
  [--cutoff X] [--window N] [--arena BYTES] [--cooloff-s S] [--refractory-s S] [--channel N] [--frame SAMPLES] [--realtime] [--json]
```

One line per file (detection times relative to the file, max probability, invocations, CPU per audio second,
arena used/size, stride) and a summary. One detector runs all files, `reset()` between them. Exit 0 when every
file after `--positives` was detected and none after `--negatives` (files before either flag are only reported),
1 otherwise, 2 on errors. WAVs must be 16 kHz 16-bit; `--channel N` picks a channel of a multi-channel recording
(the device microphone records 2). The M1 runbook uses it on a room recording.

**CPU per audio second depends on pacing.** Without `--realtime` the file is fed as fast as possible: the core
stays busy, at its top clock, with warm caches. home-companion feeds one 20 ms frame as it arrives and sleeps in between,
so each inference starts on an idle core at a low clock (cpufreq `schedutil`/`powersave`) and the same work
costs about twice the CPU time: on an x86 host 0.021 s/s flat out against 0.050 s/s paced (0.021 s/s again
when the pause between frames is spent busy-waiting, so the difference is clock and cache, not extra work;
batching 100 ms per call only gains ~7 %). `--realtime` feeds at the audio rate like home-companion, so its figure is the
one to compare with `wake_cpu_s / wake_audio_s` of home-companion's stats line (they match within a few %). The paced
figure is CPU *time*, not lost headroom: under load the governor raises the clock and the cost falls back
towards the flat-out figure.

## Tests

`cargo test -p wakeword --release` checks: manifests (v2, v1 rejected, model next to the json, name = stem);
feature scaling and the window/cool-off/refractory rules; the arena (30000 fails for `okay_alfred`, auto-sizing
gives 42912 used; `okay_nabu` uses 22448); the `okay_alfred` table in `characters/alfred/wakeword/README.md`
(11 positives within 0.1 s, `paola_alfred` must *not* trigger so a model change is noticed, 4 Italian negatives
with their M0 maximum probabilities); `okay_nabu` unchanged from M0 (6/8, misses `amy_comma` and `amy_slow`, 0/5
negatives); streaming invariance; reset; a 60 s mixed stream (11 detections, CPU per audio second < 0.05); WAV
reading. Fixtures and their licences: `fixtures/README.md`.
