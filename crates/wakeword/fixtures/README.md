<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Test fixtures

All WAVs are 16 kHz mono 16-bit PCM, synthesized by the project with [Piper](https://github.com/rhasspy/piper);
the wake word files (`alfred/`, `nabu/`) are padded with 1 s of silence before and after the phrase. Like the rest of Alfred's assets they are
**CC0-1.0** (public domain).

| Directory | What | Voices |
|---|---|---|
| `alfred/` | "alfred", "ehi alfred", "hey alfred", "okay alfred" (12 files, ~4 s each) | `en_US-amy-medium`, `en_US-lessac-medium`, `it_IT-paola-medium` |
| `negatives/` | Italian sentences without the wake word (1.5-2.3 s, not padded): `fuori1` "accendi la luce in cucina", `fuori2` "che tempo farà domani a milano", `fuori3` "raccontami una barzelletta", `ora` "che ore sono" | `it_IT-paola-medium` |
| `nabu/` | "okay nabu" positives from the M0 spike (`crates/spike-native/tools/make-wavs.sh`): plain, comma, fast, slow variants | amy, lessac |
| `okay_nabu/` | the official `okay_nabu` v2 model and manifest, a regression reference (not CC0: see below) | |

`okay_nabu/okay_nabu.json` and `okay_nabu/okay_nabu.tflite` come unmodified from
[esphome/micro-wake-word-models](https://github.com/esphome/micro-wake-word-models) `models/v2/`, commit
`40ff33f57f8fc6ad71a75ef085abbf742495b225`, under the Apache License 2.0 (`okay_nabu/LICENSE`); sha256
`6dd65604f70fe5ea9d1af73a7bf239529d1fbabc363807f45d2b22ce464ddbed` (json) and
`0689abe1912a95a3318a0d8cb2e67bad0cbcfe3e24dd6e050c75debddfb6f891` (tflite).

The 5 s of silence used by the tests is generated in the test, not committed.
