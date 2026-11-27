#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""The board's icons (crates/home-companion/src/ui/board.rs): Material Design Icons as SVG path
data, drawn by the screen with a `Path` in a 24 x 24 view box.

- `--json FILE`: every icon of @mdi/js in one compact JSON object, `{"version": "7.4.47",
  "icons": {"name": "path data", ...}}` (~2.6 MB). The release bundle carries it as
  opt/icons/mdi.json (tools/release/build-bundle.sh); the body reads it only when a tile names
  an icon that is not built in, and keeps only the paths it uses.
- `--rust FILE`: the icons built into the binary, as a sorted Rust table: every name
  `board::default_icon` returns (read from board.rs) and the ones in EXTRA (the demo states,
  the empty board). The board works without the JSON file.

Names: @mdi/js has camel-case names (`mdiBrightness5`); the kebab-case name is a best guess
(`brightness-5`, `molecule-co-2` for MDI's `molecule-co2`), so lookups compare names without
their hyphens (`board::icons::key`), which is unique across the set.

The path data comes from @mdi/js (Pictogrammers, Apache-2.0): `npm pack @mdi/js@7.4.47`, then
pass its mdi.js. mdi.js itself is never committed.

usage: make-board-icons.py PATH/TO/mdi.js [--json OUT.json] [--rust OUT.rs]
"""
import argparse
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BOARD = os.path.join(ROOT, "crates/home-companion/src/ui/board.rs")
RUST = os.path.join(ROOT, "crates/home-companion/src/ui/board/icon_table.rs")

# Built in besides the defaults: the demo states' tiles and the empty board's cells.
EXTRA = [
    "floor-lamp", "ceiling-light", "led-strip-variant", "movie-open", "window-shutter-open",
    "television", "lightbulb-group", "volume-high", "plus", "weather-night", "coffee-maker",
]


def kebab(camel):
    s = re.sub(r"(?<=[a-z0-9])(?=[A-Z])", "-", camel)
    s = re.sub(r"(?<=[A-Za-z])(?=[0-9])", "-", s)
    return s.lower()


def key(name):
    return name.replace("-", "").lower()


def read_mdi(path):
    text = open(path, encoding="utf-8").read()
    m = re.match(r"// Material Design Icons v([0-9.]+)", text)
    version = m.group(1) if m else "unknown"
    icons = {}
    for camel, d in re.findall(r'export (?:var|const) mdi(\w+)\s*=\s*"([^"]+)"', text):
        icons[kebab(camel)] = d
    if len({key(n) for n in icons}) != len(icons):
        sys.exit("two icons share a name without hyphens: lookups would be ambiguous")
    return version, icons


def defaults():
    src = open(BOARD, encoding="utf-8").read()
    body = src.split("pub fn default_icon", 1)[1].split("\n}\n", 1)[0]
    return re.findall(r'=> "([a-z0-9-]+)"', body)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("mdi_js")
    ap.add_argument("--json")
    ap.add_argument("--rust", nargs="?", const=RUST)
    a = ap.parse_args()
    version, icons = read_mdi(a.mdi_js)
    by_key = {key(n): (n, d) for n, d in icons.items()}
    if a.json:
        os.makedirs(os.path.dirname(os.path.abspath(a.json)), exist_ok=True)
        with open(a.json, "w", encoding="utf-8") as f:
            json.dump({"version": version, "icons": icons}, f, separators=(",", ":"))
        print(a.json, len(icons), "icons", os.path.getsize(a.json) // 1024, "KB")
    if a.rust:
        wanted = sorted({key(n) for n in defaults() + EXTRA})
        missing = [k for k in wanted if k not in by_key]
        if missing:
            sys.exit("not in @mdi/js: " + ", ".join(missing))
        lines = [
            "// SPDX-License-Identifier: GPL-3.0-or-later",
            "// SPDX-FileCopyrightText: 2026 Andrea Mattera",
            "",
            f"// The board's built-in icons: Material Design Icons {version} (Pictogrammers, Apache-2.0),",
            "// SVG path data from @mdi/js in a 24 x 24 view box; the keys are the names without hyphens.",
            "// Written by tools/make-board-icons.py: do not edit by hand.",
            "",
            f'pub const VERSION: &str = "{version}";',
            "",
            "/// (key, path data), sorted by key.",
            "pub const TABLE: &[(&str, &str)] = &[",
        ]
        for k in wanted:
            one = f'    ("{k}", "{by_key[k][1]}"),'
            # As rustfmt writes it: one line when the tuple fits in 60 columns (fn_call_width), else a field a line.
            lines += [one] if len(one.strip().rstrip(",")) <= 60 else ["    (", f'        "{k}",', f'        "{by_key[k][1]}",', "    ),"]
        lines.append("];")
        with open(a.rust, "w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
        print(a.rust, len(wanted), "icons")


if __name__ == "__main__":
    main()
