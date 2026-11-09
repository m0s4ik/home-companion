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
- **Do not disturb**: `poses/dnd.png`, a hand bell asleep on a velvet cushion (gpt-image-2.5-flare from `idle.png`,
  background made transparent).
- **Done**: `animations/done.png` (48 frames, 3 s), a courteous bow with the right hand on the chest after a house
  command: from `idle.png` to the bow (0-19), held (20-28), back to `idle.png` (29-47). The bow's pose was drawn with
  gpt-image-2.5-flare from `idle.png`, the two clips (idle to bow, bow to idle) with Wan 2.2 like the others.
- **Night**: `poses/sleeping.png` (drawn with gpt-image-2 from `idle.png`: eyes closed, a nightcap) at 60 % on the
  background, as the night clips are. `animations/night.png` (224 frames, made from 440x760 16 fps clips
  generated from the poses) holds `fall_asleep` (0-79, idle to sleeping, he puts on the cap while dimming to 60 %),
  the `sleeping` loop (80-143, 4 s, once a minute) and `wake_from_sleep` (144-223, he takes the cap off and
  brightens), in that order so each runs into the next; aligned on `idle.png`. `sleeping.png` matches the loop's
  first frame (mean difference 0.8/255).
- **How the animations were made**: Wan 2.2 I2V A14B (fp8, with the lightx2v 4-step LoRA; weights under
  Apache-2.0) in ComfyUI, from the poses, one clip per state.

## Improving Alfred

Redrawing him, a better animation, a pose that is missing: all welcome, even just the drawings. The sheets here
are a processed form and not a good starting point; the material they come from is in the
[Alfred source package](https://github.com/m0s4ik/home-companion/releases/tag/alfred-sources-v1): every clip's
final frames, the video model's original output, the ComfyUI workflows, the prompts of the poses and of the clips,
and the rules a new drawing must keep (canvas, feet in the same place, the flat `#03050D` background, loops that
close on their first frame). Rebuild a sheet with `tools/make-animation.py`, look at it with a headless screenshot of
its state, and open a pull request: contributions to the character are CC0 as well.

Missing today: poses for "plug it in" (the Home Assistant link page), "at work", "satisfied" and "a nod", and
animations for the offline and muted poses.
