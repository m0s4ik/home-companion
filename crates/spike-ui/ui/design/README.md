<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# The body's design system

The "quiet desk" layout of the companion screen: `tokens.slint` (colours, type
scale, grid), `components.slint` (buttons, cards, rows, dialogs, the volume HUD), `icons.slint`
and `icons/<size>/<name>.svg` (written by `tools/make-icons.py`).

Licences of the assets:

- `icons/`: Material Design Icons, Apache-2.0 (`icons/LICENSE`).
- `cover-sample.png`: an original drawing, CC0-1.0 (the music page's placeholder cover).
- `qr-setup.png`: a QR code of Home Assistant's default local address (mDNS name, port 8123;
  tools/make-icons.py does not write it), CC0-1.0: the link page's picture until the body draws the real code.
- The fonts (`../../fixtures/fonts/`): Inter, SIL Open Font License 1.1
  (`Inter-LICENSE.txt`); `Inter-*-tnum-subset.ttf` are the same subsets with the digits mapped
  to Inter's tabular figures and the family renamed "Inter Tnum" (the OFL allows modified
  versions under a different name).
