<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Third-party code

## femtovg 0.27.0 (MIT OR Apache-2.0), patched

`femtovg/` is the crates.io release of [femtovg](https://github.com/femtovg/femtovg) 0.27.0, Slint's OpenGL
renderer, with `femtovg-0.27.0-rotation.patch` applied (the patch is kept next to it, so the change is easy to
read and to re-apply on a newer release). The workspace uses it through `[patch.crates-io]` in `Cargo.toml`.

Why: the device's panel is mounted sideways, so the whole UI is drawn rotated by 270 degrees. Upstream femtovg
draws text through its glyph atlas only when the transform is a uniform scale plus a translation; under a
rotation it falls back to filled paths, which on this GPU come out blurred and slow, and glyphs above 92 px
never use the atlas. The patch:

- treats a rotation by a right angle with unit scale like a translation (the atlas is used, glyphs stay crisp);
- raises the atlas' largest glyph size from 92 to 160 px (the clock on the home screen).

Licence: femtovg's own (`femtovg/LICENSE-MIT`, `femtovg/LICENSE-APACHE`); the patch is under the same terms.
Proposing it upstream is planned.
