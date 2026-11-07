<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# spike-native (M0)

Proves the native-library plan of design sections 5 and 6 with two small binaries:

- **`mww`**: TensorFlow Lite Micro (TFLM) and the microfrontend audio front end (the C library that
  ESPHome's `micro_wake_word` uses) compiled into a Rust binary by `build.rs`. It runs an official
  microWakeWord v2 model (`okay_nabu`) over a WAV file exactly as ESPHome does, and reports
  detections, CPU time per second of audio, RSS and the tensor arena actually used.
- **`vosk-grammar`**: a Rust helper linked against the official prebuilt `libvosk.so` (vosk-api
  0.3.45). It decodes a WAV with a closed grammar, streaming 100 ms chunks like a microphone, and
  prints the result with per-word confidence, timings and the section 5 reflex decision.

Nothing downloaded is committed: TFLM, the microfrontend, flatbuffers, kissfft, libvosk, the models
and the generated WAVs are fetched by the scripts in `tools/` into git-ignored directories
(`third_party/`, `models/`, `fixtures/generated/`).

## Pinned inputs

| What | Where | Pin |
|---|---|---|
| TensorFlow Lite Micro | github.com/tensorflow/tflite-micro | commit `964e43d7ee3e82fe6abb6075b7f50506eafce79b` (2026-11-02) |
| microfrontend (`tensorflow/lite/experimental/microfrontend/lib`) | github.com/tensorflow/tensorflow, sparse checkout | commit `e166dba74d0ff24174457522e3bf5655a117ab40` |
| flatbuffers (headers) | v25.9.23 zip | md5 `023eca1e211d64007124420cd6be29c7` (as TFLM pins it), sha256 `083a4249…1fb816` |
| kissfft | 131.2.0 zip | md5 `c1c39842615fdbe0c318f035b8322372` (as TFLM pins it), sha256 `0fd8757f…6121c1` |
| okay_nabu v2 model + manifest | github.com/esphome/micro-wake-word-models `models/v2/` | commit `40ff33f57f8fc6ad71a75ef085abbf742495b225`, sha256 checked by the script |
| libvosk 0.3.45 x86_64 / aarch64 | github.com/alphacep/vosk-api releases | sha256 `bbdc8ed8…5fbb2` / `45e95d37…94541` (checked) |
| Vosk Italian model | `~/prove-vosk/vosk-model-small-it-0.22` (symlinked to `models/vosk-it`) | `VOSK_MODEL` overrides |

### Why not `create_tflm_tree.py`

The plan was to run TFLM's `create_tflm_tree.py`. It runs the TFLM Makefile, which runs Python code
from the downloaded tree (`download_dependencies.py`, `specialize_files.py`); executing downloaded
code was refused on the build machine, so the documented fallback is used instead and is simpler:

- `tools/fetch-tflm.sh` only downloads (git, curl) and checks the two archives against the md5 sums
  in the pinned `download_dependencies.py`. Since 2026 TFLM needs only flatbuffers and kissfft
  (gemmlowp and ruy are gone).
- `build.rs` selects the sources the way the pinned Makefile does for a host build: the
  `MICROLITE_CC_BASE_SRCS` wildcards (`tensorflow/lite/micro/*.cc`, `arena_allocator/`,
  `memory_planner/`, `kernels/internal/`) and the `MICROLITE_CC_KERNEL_SRCS` list read from
  `tools/make/sources.inc` (all reference kernels), minus tests, benchmarks and test helpers.
  The signal library is not needed. Unused kernels cost nothing: the linker only keeps what the 20
  registered ops reference.

## Build

```sh
export PATH=$HOME/.cargo/bin:$PATH CARGO_BUILD_JOBS=2
crates/spike-native/tools/fetch-tflm.sh     # TFLM, microfrontend, flatbuffers, kissfft
crates/spike-native/tools/fetch-vosk.sh     # libvosk x86_64 + aarch64, models/vosk-it symlink
crates/spike-native/tools/fetch-models.sh   # okay_nabu.tflite + v2 manifest
cargo build -p spike-native --release
PKG_CONFIG_SYSROOT_DIR=/ PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig PKG_CONFIG_ALLOW_CROSS=1 \
  cargo build -p spike-native --release --target aarch64-unknown-linux-gnu
cargo test -p spike-native --release
```

All three fetch scripts are idempotent (a second run does no network work) and print the sha256 of
what they use. `build.rs` stops with a message naming the script to run when `third_party/` is
missing. C and C++ go through the `cc` crate, which picks `aarch64-linux-gnu-gcc/g++` from the
target triple. A full build compiles about 150 C++ files and takes about 3 minutes per target with
2 jobs.

What `build.rs` builds (static archives, linked in this order):

| Archive | Contents | Flags |
|---|---|---|
| `libtflm.a` | TFLM core + reference kernels + `src/mww_shim.cc` | C++17, `-O2 -fno-exceptions -fno-rtti -fno-threadsafe-statics`, `-DTF_LITE_STATIC_MEMORY -DTF_LITE_DISABLE_X86_NEON -DTFLM_REMOVE_VIRTUAL_DELETE -DFLATBUFFERS_LOCALE_INDEPENDENT=0 -DFLATBUFFERS_PREFER_PRINTF` |
| `libmicrofrontend.a` | `frontend*.c window*.c filterbank*.c noise_reduction*.c pcan_gain_control*.c log_scale*.c log_lut.c` (no `*_io.c`, `*_main.c`, memmap) | C11, `-O2` |
| `libmfft.a` | the frontend's `fft.cc`, `fft_util.cc` (they `#define FIXED_POINT 16` themselves) | C++17 |
| `libkissfft16.a` | `kiss_fft.c`, `kiss_fftr.c` | C11, `-DFIXED_POINT=16` |

No symbol clashes: TFLM's own kissfft wrappers belong to the signal library, which is not built.
`libvosk.so` is linked only into `vosk-grammar` (`#[link(name = "vosk")]` in `src/vosk_ffi.rs`,
which only that binary compiles), with `rpath $ORIGIN` so the device copy finds `libvosk.so` next
to the binary.

## Run

```sh
cd crates/spike-native
../../target/release/mww --model models/okay_nabu.tflite --manifest models/v2/okay_nabu.json \
    --wav fixtures/generated/okay_nabu_lessac_plain.wav [--cutoff 0.97] [--window 5] [--arena 26080] \
    [--cooloff-s 1.0] [--channel N] [--dump-features] [--quiet]
LD_LIBRARY_PATH=third_party/vosk/x86_64 ../../target/release/vosk-grammar --model models/vosk-it \
    --grammar fixtures/grammar-it.json --wav fixtures/generated/vosk/ora.wav [--chunk-ms 100] [--min-conf 0.8]
```

`mww` exits 0 when it detects, 1 when not, 2 on error; the last line is `RESULT {json}` (detections
with timestamps, max probability, audio/CPU seconds, `cpu_per_audio_second`, RSS, arena used).
`--dump-features` prints every feature slice (raw front-end values and the int8 model input).
Both binaries need 16 kHz S16 input; a multi-channel file (the device microphone has 2) needs
`--channel N`.

`vosk-grammar` exits 0 when the result is accepted as a reflex, 1 when not, 2 on error. The first
line is JSON: `text`, `words` (`word`, `conf`, `start`, `end`), `accepted`, `min_word_conf`,
`model_load_s`, `decode_s`, `final_ms` (time of the final-result call after the last chunk), `rtf`,
`cpu_s`, `rss_mb`; the second line is a summary. If Vosk ends an utterance in the middle of the
file, the segments are joined.

### How `mww` follows ESPHome

- Front end as `preprocessor_settings.h`: 30 ms window, step from the manifest (10 ms), 40
  channels 125-7500 Hz, noise reduction (smoothing bits 10, even 0.025, odd 0.06, min signal
  0.05), PCAN (strength 0.95, offset 80, gain bits 21), log scale (shift 6), 16 kHz.
- Ops: exactly ESPHome's 20 streaming ops in a `MicroMutableOpResolver<20>`; resource variables in
  their own arena (ESPHome: 1024 bytes on the 32-bit ESP32; scaled by pointer size here, 2048
  bytes on 64-bit, otherwise `MicroResourceVariables` runs out of memory and Invoke crashes).
- Quantization: `int8 = round(feature * 256 / 666) - 128`, clamped (666 ~ 25.6 * 26.0, the float
  scale used in training); slices written at `stride_step % stride`, Invoke when the stride is
  full; output is a uint8 probability.
- Detection: sum of the last N probabilities > `round(cutoff * 255) * N`; after a detection the
  window is cleared and, as ESPHome's `MIN_SLICES_BEFORE_DETECTION = 100`, the rule is not checked
  for 1 s (33 invocations with stride 3; also at start), so one utterance gives one detection.

## Tests

- `cargo test -p spike-native --release`: manifest parsing, ESPHome feature scaling (0 -> -128,
  332 -> 0, 670 -> 127, and agreement with the float formula), the sliding-window rule and cool-off,
  the grammar acceptance rule.
- `tools/make-wavs.sh`: Piper (`~/prove-vosk/venv`) synthesizes "okay nabu" with
  `en_US-lessac-medium` (plain, comma, length scale 0.9/1.0/1.2) and `en_US-amy-medium` (plain,
  comma, slow) at 22050 Hz; sox converts them to 16 kHz mono S16 with 1 s of silence before and
  after; copies the Italian negatives `fuori1..3.wav`, `ora.wav` and the 12 Vosk sentences with
  `frasi.txt`; makes 5 s of silence. **Added apt package: `sox`** (`tools/resample16k.py`, linear
  interpolation, is the fallback without it).
- `tools/run-mww-tests.sh`: table over positives and negatives plus a 60 s concatenation for the CPU
  figure; fails unless at least one positive is detected and no negative is.
- `tools/run-vosk-tests.sh`: table over the 12 sentences; fails unless at least 8 of the 9
  commands give exactly the expected phrase and the RTF is below 0.5.

Both test scripts take `RUNNER` (e.g. `"nice -n 10 taskset -c 3"`, or a qemu prefix) and find the
binaries in `../../target/release/` or, on the device, in `bin/` next to `tools/`.

## Results on the build machine (x86_64, 4 vCPU, other builds running)

`tools/run-mww-tests.sh`, manifest cutoff 0.97, window 5:

| File | Kind | Detected | At (s) | Max prob | CPU s / audio s |
|---|---|---|---|---|---|
| okay_nabu_amy_comma | positive | no | - | 0.651 | 0.0095 |
| okay_nabu_amy_plain | positive | yes | 2.27 | 1.000 | 0.0099 |
| okay_nabu_amy_slow | positive | no | - | 0.980 | 0.0099 |
| okay_nabu_lessac_comma | positive | yes | 2.21 | 1.000 | 0.0098 |
| okay_nabu_lessac_comma_slow | positive | yes | 2.72 | 0.996 | 0.0094 |
| okay_nabu_lessac_fast | positive | yes | 1.91 | 1.000 | 0.0076 |
| okay_nabu_lessac_plain | positive | yes | 1.94 | 1.000 | 0.0055 |
| okay_nabu_lessac_slow | positive | yes | 2.03 | 1.000 | 0.0058 |
| silence | negative | no | - | 0.043 | 0.0085 |
| fuori1 / fuori2 / fuori3 / ora | negative | no | - | 0.043 | 0.0075-0.0099 |
| concat60 (61.9 s) | cpu | 11 detections | | 1.000 | **0.0081** |

6 of 8 positives detected, 0 of 5 negatives; on the 60 s file 0.50 s of CPU for 61.9 s of audio
(**0.8 % of one x86_64 core**, expectation < 2 %), peak RSS 6.6 MB, arena **22448 of 26080 bytes**
(the manifest size is enough on 64-bit), model stride 3 (one Invoke every 30 ms). Front-end values
on a positive: 0-658, i.e. 0-25.7 after dividing by 25.6, the 0-26 range ESPHome expects.
The per-file figures of a few seconds are noisy (getrusage tick, concurrent builds); use the 60 s
file.

`tools/run-vosk-tests.sh` (x86_64 libvosk, `vosk-model-small-it-0.22`, `fixtures/grammar-it.json`):

| File | Said | Got | Exact | Accepted (0.8) | Min word conf | RTF | Final ms |
|---|---|---|---|---|---|---|---|
| stop | stop | [unk] | NO | no | 1.00 | 0.03 | 10 |
| volume_su | alza il volume | alza il volume | yes | yes | 1.00 | 0.02 | 5 |
| volume_giu | abbassa il volume | abbassa il volume | yes | yes | 1.00 | 0.02 | 5 |
| ora | che ore sono | che ore sono | yes | yes | 1.00 | 0.02 | 9 |
| timer5 | metti un timer di cinque minuti | same | yes | yes | 1.00 | 0.02 | 8 |
| timer10 | imposta un timer di dieci minuti | same | yes | yes | 1.00 | 0.02 | 8 |
| sveglia | svegliami alle sette e mezza | same | yes | yes | 1.00 | 0.02 | 9 |
| annulla | annulla il timer | annulla il timer | yes | yes | 1.00 | 0.03 | 7 |
| silenzio | silenzio | silenzio | yes | yes | 1.00 | 0.02 | 10 |
| fuori1 | accendi la luce in cucina | cinque | neg | no | 0.93 | 0.02 | 10 |
| fuori2 | che tempo farà domani a milano | che timer | neg | no | 0.83 (che 0.96, timer 0.83) | 0.02 | 4 |
| fuori3 | raccontami una barzelletta | un abbassa il | neg | no | 0.56 | 0.03 | 7 |

8 of 9 commands exact ("stop" -> `[unk]`, as in the design's measurement), RTF 0.024 over 22.5 s,
model load 0.45-0.75 s, peak RSS 178 MB. The negatives are all rejected by the exact-phrase rule,
but note what the grammar does to them: Vosk's grammar admits **any sequence of grammar words**,
not only whole phrases ("che timer", "un abbassa il"), and single words with high confidence
("cinque" 0.93). Confidence alone would not have rejected fuori1 or fuori2 at 0.8; the
exact-phrase check did. This is the data for the M3 threshold discussion.

## Binaries

| | x86_64 (as built) | aarch64 (as built) | aarch64 stripped |
|---|---|---|---|
| `mww` | 16.9 MB | 16.6 MB | **0.73 MB** |
| `vosk-grammar` | 16.7 MB | 5.8 MB | **0.53 MB** (+ `libvosk.so` 7.5 MB) |

"As built" carries line tables (`debug = "line-tables-only"`, also passed to the C++ compiles);
`aarch64-linux-gnu-strip` gives the size that matters on the device.

Dynamic dependencies of the aarch64 binaries (`aarch64-linux-gnu-readelf -d`):

```
mww:           NEEDED libstdc++.so.6, libgcc_s.so.1, libm.so.6, libc.so.6          (no libvosk)
vosk-grammar:  NEEDED libvosk.so, libstdc++.so.6, libgcc_s.so.1, libm.so.6, libc.so.6
               RUNPATH $ORIGIN
libvosk.so:    NEEDED libatomic.so.1, libstdc++.so.6, libm.so.6, libgcc_s.so.1, libc.so.6
               (needs at most GLIBC_2.17, GLIBCXX_3.4.18: fine on Debian 13)
```

`libatomic.so.1` (package `libatomic1`) must be on the device image for `libvosk.so`.

### Under qemu (bonus)

`qemu-aarch64 -L /usr/aarch64-linux-gnu`: the aarch64 `mww` gives exactly the x86_64 detections
and probabilities on all 13 files (`MWW=../../target/aarch64-unknown-linux-gnu/release/mww
RUNNER="qemu-aarch64 -L /usr/aarch64-linux-gnu" tools/run-mww-tests.sh`: 6/8 positives, 0/5
negatives, PASS; emulated CPU figures are meaningless). The aarch64 `vosk-grammar` with
`LD_LIBRARY_PATH=third_party/vosk/aarch64` decodes `timer5.wav` to "metti un timer di cinque
minuti", accepted, all words conf 1.00 (load 8.6 s, RTF 1.9 under emulation).
