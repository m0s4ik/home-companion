<!-- SPDX-License-Identifier: CC0-1.0 -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Alfred

A friendly elderly butler in a tailcoat, white gloves and bow tie; wake word "Okay Alfred". The first character
of home-companion.

- **Poses**: 1024x1024 RGB on a flat very dark navy background, drawn with OpenAI's image model (gpt-image-2)
  from one base picture (`idle.png`, hands behind the back), one pose per state derived from it. `offline` is
  Alfred in a deck chair under a parasol, `muted` wears ear muffs.
- **Licence**: CC0-1.0 (public domain), pictures and phrases.
- **Animations**: made from the previous Python screen's frames (440x760 RGB, 16 fps, ~60 MB of PNG) with
  `tools/make-animation.py`: frames cut to where the figure moves and kept at their own resolution (the body scales
  them), background noise snapped to the flat background, and only the pixels that change by more than 6 levels
  from one frame to the next stored: 9.7 MB in all. The loading clip (208 frames) contains the idle loop (frames
  0-63) and the wake-up, where he fixes his bow tie and glove (96-175), so the three share `animations/loading.png`.
- **Night**: `poses/sleeping.png` (drawn with gpt-image-2 from `idle.png`: eyes closed, a nightcap) at 60 % on the
  background, as the night clips are. `animations/night.png` (224 frames, made from 440x760 16 fps clips
  generated from the poses) holds `fall_asleep` (0-79, idle to sleeping, he puts on the cap while dimming to 60 %),
  the `sleeping` loop (80-143, 4 s, once a minute) and `wake_from_sleep` (144-223, he takes the cap off and
  brightens), in that order so each runs into the next; aligned on `idle.png`. `sleeping.png` matches the loop's
  first frame (mean difference 0.8/255).
