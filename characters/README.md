<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Characters

A character is the face and the words of the companion: poses, colours, phrases. The body (screens, layout,
fonts, icons, audio, brains) is the same for every character. Every character lives in this directory, one
directory each; a new one is added with a pull request.

```
characters/<id>/
  character.toml      metadata, colours, pose crop, phrases (format below)
  poses/<state>.png   one picture per state
  animations/*.png    optional: animation sheets (below)
  README.md           who made it, how (tools, prompts), anything a maintainer should know
```

Later formats add `sounds/` and `phrases/<lang>/*.wav` (design section 4); like the animations they are optional
and a character without them still works.

## States and poses

| State | File | Required | Shown when |
|---|---|---|---|
| `idle` | `poses/idle.png` | yes | waiting for the wake word (home screen) |
| `listening` | `poses/listening.png` | yes | after the wake word, while the user speaks |
| `thinking` | `poses/thinking.png` | yes | waiting for the brain's answer |
| `speaking` | `poses/speaking.png` | yes | the answer is spoken |
| `error` | `poses/error.png` | yes | not understood, or the brain failed |
| `offline` | `poses/offline.png` | no (falls back to `idle`) | no brain is reachable |
| `muted` | `poses/muted.png` | no (falls back to `idle`) | the microphone switch is off |
| `timer_finished` | `poses/timer_finished.png` | no (falls back to `speaking`) | a timer or alarm is ringing |
| `sleeping` | `poses/sleeping.png` | no (falls back to `idle`, drawn dimmed) | night mode: the body's night hours, the home at rest. Drawn as it is: make it as dark as it should be at night |

Rules for the pictures:

- PNG, 8 bits per channel (RGB or RGBA), **all the same size** within a character (Alfred: 1024x1024).
- The figure stands in the same place and at the same scale in every pose, so it does not jump between states:
  compared with `idle`, the figure's height (head to feet) within 2%, the feet at the same height and centred on the
  same x within 1.5% of the picture height. Poses with their own crop (`[crop_override]`, a different scene such as
  Alfred's deck chair) are not compared, nor is `sleeping` when the `fall_asleep` animation carries the figure there.
- The background is one flat colour, equal to `background` in `character.toml` (the screen fills the rest with
  it). With RGBA, transparent pixels show that colour.
- `crop` in `character.toml` is the part of the picture that holds the figure; the body draws it as tall as the
  character area of the screen (740 px in landscape), keeping the scale the same for every pose. A pose that needs
  more room (Alfred's deck chair) gets its own crop in `[crop_override]`, with the same height.
- No text in the pictures (the body writes in the user's language).

## Animations

All optional; without one the still pose is shown.

| Name | Plays | Starts from |
|---|---|---|
| `loading` | in a loop while the body starts, until Home Assistant connects | `idle` |
| `wake` | once when the home screen wakes up (touch, volume key, movement, end of a conversation) | `idle` |
| `idle` | after `wake`, again and again while the screen is awake, still on its last frame for `pause` seconds in between | `idle` |
| `listening`, `thinking`, `speaking`, `error` | in a loop while the state lasts (`speaking` also while a timer rings, unless the character has `timer_finished`) | that state's pose |
| `fall_asleep` | once when the night layout comes (night hours, the home at rest), after the home clip under way | `idle` |
| `sleeping` | once every `pause` seconds while asleep, the still `sleeping` pose in between (nothing redraws) | `sleeping` |
| `wake_from_sleep` | once when the screen wakes up at night, or the night ends; then the home as by day, without `wake` | `sleeping` |

`fall_asleep` and `wake_from_sleep` are **transitions**: they start on one pose and end on another (their last frame
left out, so the next picture follows without a repeat). They are decoration: a conversation cuts them at once.

An animation is a **sheet**: one PNG, 8-bit RGBA, the frames stacked from top to bottom, all the same size.
**Frame 0 is opaque** (the whole picture); in every following frame a pixel is **opaque where it changes** from
the previous frame and **transparent (alpha 0) where it stays the same**; no other alpha values. The body keeps
only the changed pixels in memory and redraws only what moves. Several animations may share a sheet (each takes a
range of its frames). Frames show the figure and its flat background (the `background` colour) only, cut to the
box where the figure moves.

```toml
[animations.idle]
file = "animations/loading.png"  # in animations/
size = [288, 604]                 # one frame's width and height in the sheet
frames = [0, 64]                  # frames first..end-1 of the sheet
fps = 16
pause = 5                         # optional, seconds still on the last frame before playing again
rect = [292.4, 40.2, 742.7, 985.1] # x0, y0, x1, y1 in the pose pictures: where the frames are drawn
```

`rect` puts the frames over the pose they start from, so the figure does not jump when the body switches between
the pose and the animation. [tools/make-animation.py](../tools/make-animation.py) makes the sheet from a directory
of frames (`frame_000.png`, …), finds `rect` by matching frame 0 with the pose and prints the table.

## Text

Phrases and names use only the characters the screen's fonts carry: Latin-1 (U+0020-U+00FF, which covers
Italian, French, German, Spanish…) plus `…` `–` `—` `•`. For quotes use `«»` or `"`.

## Sounds

`sounds/<name>.wav`: the body's sounds (wake, error, the reflexes' own sounds, the bell, the tap under the finger…),
listed in `alfred/sounds/README.md`. Optional one by one: a missing file is a plain tone.

## Stop word

`stopword/<name>.json` (+ its `.tflite`): an optional microWakeWord model ("basta", "stop") the body listens to only
while the character thinks, speaks or rings; it works as the Stop button. The first `*.json` alphabetically is used.

## Reflexes

`grammar/<lang>.toml` (optional): the phrases the body does by itself without Home Assistant: `time`, `timer_start`, `timer_cancel`, `timer_left`, `alarm_set`, `alarm_cancel`, `volume_up`, `volume_down`,
`volume_set`, `volume_max`, `volume_min`, `silence`, each a list of phrases; `timer_start` needs `{duration}`,
`alarm_set` `{time}`, `volume_set` `{percent}` (their spoken forms per language are in `crates/reflex/src/slots.rs`).
Lower case, as Vosk writes it; every word must be in the language model's vocabulary (Vosk logs the missing ones). A
phrase that could mean two commands is an error. Without the file the character has no reflexes in that language.
See [alfred/grammar/it.toml](alfred/grammar/it.toml).

## character.toml

```toml
format = 1                       # version of this format
id = "alfred"                    # = directory name: lowercase letters, digits, '-'
name = "Alfred"
license = "CC0-1.0"              # SPDX id of the pictures and phrases: CC0-1.0, CC-BY-4.0 or CC-BY-SA-4.0
authors = ["Andrea Mattera"]
languages = ["en", "it"]         # every language listed needs a [phrases.<lang>] table; English first
                                 # (the default), then the others; a missing language uses the first
wake_word = "Okay Alfred"        # as shown on screen

crop = [212, 0, 812, 1024]       # x0, y0, x1, y1 in the pose pictures
mouth = [470, 150, 570, 210]     # the mouth in poses/idle.png, inside crop: a double tap there opens the
                                 # settings panel (make it a bit larger than the drawing, for a finger)

[crop_override]                  # optional, per state
offline = [128, 0, 888, 1024]

[colors]                         # "#rrggbb"
background = "#03050d"
idle = "#5a6e8c"
listening = "#28c8e6"
thinking = "#966ef0"
speaking = "#f5aa3c"
error = "#eb4646"

[phrases.en]                     # all keys required
idle = "Say \"Okay Alfred\""
listening = "I'm listening…"
thinking = "Let me think…"
error = "I didn't understand"
offline = "I'm on a break: I can't reach home"

[phrases.it]
idle = "Di' «Okay Alfred»"
# …
```

## Checking a character

```sh
cargo test -p character
```

loads every directory here and checks the rules above (required poses present, same size, figure aligned, crop
inside the picture, colours, phrases for every language, animation sheets). A pull request with a new character must pass it.

## Licence

The code of this repository is GPL-3.0-or-later; a character's pictures and phrases carry the licence in its
`character.toml` (Alfred: CC0-1.0, public domain).
