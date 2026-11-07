<!-- SPDX-License-Identifier: CC0-1.0 -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Alfred's wake word: `okay_alfred`

A [microWakeWord](https://github.com/kahrendt/microWakeWord) v2 model for "Okay Alfred", run on the device by
the `wakeword` crate (TensorFlow Lite Micro, ESPHome's detection rules).

| | |
|---|---|
| Made by | **Tater Totterson** |
| Source | https://github.com/TaterTotterson/Tater-Wake-Words, model `microWakeWordsV8/okay_alfred` |
| Licence | **MIT** (see `LICENSE`); the rest of Alfred is CC0 |
| `okay_alfred.json` | the v2 manifest, unmodified; sha256 `49a56fe914cf43502d4ce5bba06076f7fdbccb5502391a0670537924b2645efa` |
| `okay_alfred.tflite` | the model, unmodified (137984 bytes, ~135 KB); sha256 `d983a3a887646df1b7d9f787a6ad0f40ea87a0131cfbf24a5ad2cb0ee83391a1` |
| Manifest values | phrase "okay alfred", trained language `en`, cutoff 0.96, sliding window 5, 10 ms feature step, stride 2 (one inference every 20 ms) |

**Tensor arena.** The manifest declares `tensor_arena_size: 30000`, which is too small: the model needs
**42912 bytes** (`AllocateTensors` fails at 30000). The body does not trust the figure: the detector starts at
max(manifest, 32 KiB) and grows the arena by 1.5x until the model fits (49152 bytes for this model), and logs the
size and the bytes used.

## How a character points at its model

Today, by path convention: home-companion's configuration key `wake_word.model` names a manifest; when it is empty (the
default) home-companion uses `<character.dir>/wakeword/<first *.json>` in alphabetical order. The model id announced to
Home Assistant over Wyoming is the manifest's file stem (`okay_alfred`) and the phrase it shows is the manifest's
`wake_word`; the phrase on the screen is `wake_word` in `character.toml`.

## Measurements

Synthetic Piper voices (`crates/wakeword/fixtures/`, 1 s of silence before and after), the manifest's cutoff
(0.96) and window (5), as checked by `cargo test -p wakeword --release` on every build:

| File | Detected | At (s) | Max probability |
|---|---|---|---|
| amy_okay_alfred / amy_hey_alfred / amy_ehi_alfred / amy_alfred | yes x4 | 2.60 / 2.30 / 2.52 / 2.26 | 1.000 |
| lessac_okay_alfred / lessac_hey_alfred / lessac_ehi_alfred / lessac_alfred | yes x4 | 2.48 / 2.30 / 2.40 / 2.16 | 1.000 |
| paola_okay_alfred / paola_hey_alfred / paola_ehi_alfred (Italian voice) | yes x3 | 2.32 / 2.34 / 2.28 | 1.000 |
| paola_alfred (bare name, Italian voice) | no | – | 0.137 |
| Italian sentences fuori1 / fuori2 / fuori3 / ora ("che ore sono") | no x4 | – | 0.502 / 0.024 / 0.024 / 0.914 |
| 60 s: all of the above + 5 s of silence, one stream | 11 detections, as expected | | |

Notes:

- "Ehi Alfred" (Italian pronunciation) is detected with all three voices, the Italian one included.
- The bare name "Alfred" triggers with the English voices: a possible source of false accepts when Alfred is
  mentioned in conversation. To be measured with real voices on the device (M1 runbook).
- `ora.wav` reaches 0.914 on one inference and stays under the cutoff only thanks to the window rule (five
  consecutive inferences must average above 0.96).
- CPU: about 0.02-0.04 s per second of audio on the x86_64 build machine (a loaded 4-core container); the M0
  spike measured 3.2 % of one Cortex-A53 core for `okay_nabu` on the device; on the device it is still to be measured.
