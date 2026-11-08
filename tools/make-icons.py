#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Writes the body's icons as SVG files, one per (name, size), and icons.slint: a global of
`image` properties (`Icons.home-48`), into crates/spike-ui/ui/design/.

Slint draws an SVG `Image` by rasterizing it with resvg at the item's size (femtovg and the
software renderer with embedded files: at run time), then `colorize` tints it. One file per size
keeps the name honest (`Icons.timer-32` is drawn 32 px) and lets either renderer draw the same
pixels.

Icon rule: filled Material Design Icons (no outline variants, no brand
logos), sized for the distance they are read from. The path data comes from @mdi/js
(Pictogrammers, Apache-2.0): `npm pack @mdi/js@7.4.47`, then pass its mdi.js.

usage: make-icons.py PATH/TO/mdi.js
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
UI = os.path.join(ROOT, "crates/spike-ui/ui/design")

# name -> sizes used. 48: the action band and tiles (seen across the room); 44: inside the gold
# disc; 40: the page header; 32: cards, weather, status; 28: rows, the HUD, in-card controls;
# 24: inside the timer ring, captions; 64: a notice.
USED = {
    # action band
    "view-dashboard": [48, 32, 28], "dots-horizontal": [48], "home": [48, 44, 28],
    "message-plus": [48], "delete": [48], "close": [48, 36], "timer-plus": [48],
    "alarm-plus": [48], "skip-previous": [48, 36], "skip-next": [48, 36], "translate": [48, 28],
    "refresh": [44], "pause": [44, 28, 24], "play": [44, 28], "plus": [48, 36],
    "minus": [36], "check": [44, 28],
    # tiles, headers, rows
    "forum": [48, 32], "music-note": [48, 32], "timer": [48, 32, 24], "cog": [48], "chevron-left": [40],
    "chevron-right": [28], "chevron-up": [48, 36], "brain": [48, 28], "account-voice": [48, 28],
    "volume-high": [28], "volume-medium": [28], "volume-low": [28], "volume-off": [28],
    "brightness-6": [28], "information": [28], "qrcode": [28], "cast-audio": [32, 24],
    "cast-connected": [32, 28],
    "power-sleep": [48, 28], "weather-night": [28], "wifi": [32, 28], "wifi-off": [32, 28],
    # the control center's tiles
    "bluetooth": [32], "bluetooth-off": [32], "microphone": [44, 32],
    # cards, status, dialogs
    "bell": [32], "bell-off": [32, 28], "help-circle": [48], "cloud-off": [32], "microphone-off": [32], "alert-circle": [64, 32],
    "check-circle": [32], "cart": [48, 32], "calendar-clock": [48, 32], "alarm": [40, 32, 28, 24], "cellphone": [28], "timer-sand-full": [48],
    # weather (the date line)
    "weather-sunny": [32], "weather-night-partly-cloudy": [32], "weather-partly-cloudy": [32],
    "weather-cloudy": [32], "weather-rainy": [32], "weather-pouring": [32],
    "weather-snowy": [32], "weather-snowy-rainy": [32], "weather-fog": [32],
    "weather-hail": [32], "weather-lightning": [32], "weather-lightning-rainy": [32],
    "weather-windy": [32], "weather-windy-variant": [32],
}
USED["weather-night"] = [32, 28]


def main():
    js = open(sys.argv[1]).read()
    mdi = {re.sub(r"(?<!^)(?=[A-Z0-9])", "-", k[3:]).lower(): v
           for k, v in re.findall(r'export var (mdi\w+) = "([^"]+)";', js)}
    lines = [
        "// SPDX-License-Identifier: GPL-3.0-or-later",
        "// SPDX-FileCopyrightText: 2026 Andrea Mattera",
        "",
        "// The body's icons: Material Design Icons 7.4.47 (Pictogrammers, Apache-2.0; the SVG files",
        "// in icons/ carry that licence, see icons/LICENSE), filled glyphs, one SVG per size in",
        "// icons/<size>/<name>.svg. Written by tools/make-icons.py: do not edit by hand. Draw them",
        "// with `Icon` (components.slint): an Image with `colorize`, at exactly the size in the name.",
        "",
        "export global Icons {",
    ]
    n = 0
    for name in sorted(USED):
        for size in sorted(USED[name], reverse=True):
            d = os.path.join(UI, "icons", str(size))
            os.makedirs(d, exist_ok=True)
            with open(os.path.join(d, f"{name}.svg"), "w") as f:
                f.write(f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" '
                        f'viewBox="0 0 24 24"><path fill="#fff" d="{mdi[name]}"/></svg>\n')
            lines.append(f'    out property <image> {name}-{size}: '
                         f'@image-url("icons/{size}/{name}.svg");')
            n += 1
    lines.append("}")
    with open(os.path.join(UI, "icons.slint"), "w") as f:
        f.write("\n".join(lines) + "\n")
    print(n, "icon files")


if __name__ == "__main__":
    main()
