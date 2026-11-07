<!-- SPDX-License-Identifier: CC0-1.0 -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Alfred's stop words: "basta" and "stop"

microWakeWord v2 models (int8, streaming) that the body runs **only while Alfred thinks (a wrong transcript
on the screen), speaks, rings or makes an announcement** (`[wake_word] stop_model`; by default every `*.json` here, and any of them stops). A detection works
as the Stop button. Because they listen only a few minutes a day, the operating points favour recall over a very
low false-accept rate. Trained and measured on the DGX Spark, 2026-11-05/06; the full curves (every sliding window
1-10 and cutoff 0.30-0.99) are in `curves/*.csv`.

| Model | What | Window / cutoff | Recall clean / noisy | False accepts per hour: Italian speech / English party / Alfred's voice |
|---|---|---|---|---|
| `stop.*` | ESPHome's official "Stop" (Kevin Ahrendt), unmodified, recalibrated | 10 / 0.80 | 0.71 / 0.58 | 1.92 / 0.27 / 0.95 |
| `stop_alfred.*` | trained here (stop3) | 10 / 0.89 | 0.68 / 0.57 | 1.92 / 0 / 0 |
| `basta.*` | trained here (basta2) | 2 / 0.90 | 0.49 / 0.31 | 4.81 / 0.13 / 0.95 |

- **The two "stop"s complement each other**: the official one hears a bare "stop" / "stop!" (1.00) but rarely
  "Alfred, stop!" (0.27); `stop_alfred` hears "Alfred, stop!" (0.89) and "ok stop" (0.86) but a bare "stop" only half
  of the time (0.48). Both are loaded.
- **"basta" is the weak one**: /ast/ and /sta/ are everywhere in Italian (questa, tempesta, resta, basterà…). Its
  manifest is moved from the delivered 2-per-hour point (window 10, cutoff 0.78, recall 0.39 / 0.26) to the
  5-per-hour row (window 2, cutoff 0.90); on 253 real recordings with "basta" inside a read sentence it fires 27 %
  of the time (control without the word: 1.8 %). It works best with an onset: "Alfred, basta!" 0.62, "adesso
  basta" 0.53, a bare "basta" 0.23. Recordings of the household's own voice are the next step.
- **How it was measured**: recall on synthetic phrases from voices never used in training (Piper it/es/pt/en), clean
  and mixed with music and noise at 5-15 dB SNR, fed as on the device; false accepts on 10.4 h of Italian read
  speech (Multilingual LibriSpeech dev+test), 15 h of microWakeWord's `dinner_party_eval` and 1.05 h of assistant
  answers read by `it_IT-paola-medium` (Alfred's voice, never a positive); 1 s refractory after a detection.
- **tensor_arena_size**: 30000 in the manifests of the models trained here (estimated, not measured with TFLite
  Micro); the official one keeps its 21000. The body does not rely on it: the detector grows the arena until the
  model fits (on the device: `stop_alfred` 24624 bytes used).

## How they were made

microWakeWord (commit 4665173), mixednet, stride 3, 20 000 + 10 000 steps. Positives: piper-sample-generator with
Piper voices (it, es, ca, pt, ro; en for "stop") and LibriTTS-R (800 interpolated speakers), Alfred's own voice
excluded; augmentation with the MIT impulse responses, AudioSet/FMA noise, Italian background speech, EQ, pitch and
distortion. Negatives: microWakeWord's negative sets, about 60 h of Italian MLS speech, 67 confusable words read by
Italian voices, 1 500 assistant sentences in Alfred's voice, and windows of 29 800 Italian recordings where the first
models fired by mistake. No household data: only synthetic voices and public datasets.

## Licences

- `stop.tflite` / `stop.json`: Apache-2.0 (ESPHome / Kevin Ahrendt), see `LICENSE-stop`; the manifest's window and
  cutoff are changed.
- `basta.*`, `stop_alfred.*`, the curves: CC0 like the rest of Alfred, **but check before any redistribution**:
  some Piper voices used for training come from datasets for non-commercial use only (e.g. `en_US-lessac`), the MIT
  impulse responses are for research use, AudioSet and FMA clips have mixed licences, LibriTTS-R and MLS are
  CC BY 4.0. Fine for a home device; to be reviewed before this repository or these files are published.
