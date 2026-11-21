#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Makes the files of a scenes release (Settings → Scenes, src/scenes.rs): the scenes of a
directory, renamed to the release's asset names, and the catalogue `scenes.json` with each file's
size and SHA-256.

usage: make-scene-catalogue.py SCENES_DIR CHARACTER OUT_DIR [--titles TITLES_JSON]

SCENES_DIR holds `<name>.png` per scene, and `<name>.toml` + `<name>-anim.png` for one that moves
(characters/README.md, "scenes"). The assets are `<character>-<file>`, so one release can carry
the scenes of every character. TITLES_JSON: {"<name>": {"en": "…", "it": "…"}}; a scene without a
title is shown by its name. Then publish OUT_DIR's files as a release whose tag starts with
`scenes-` (e.g. `gh release create scenes-v1 OUT_DIR/*`) in the repository of `[update] repo`.
"""
import argparse
import hashlib
import json
import re
import shutil
import sys
from pathlib import Path

NAME = re.compile(r"^[a-z0-9_-]{1,40}$")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("scenes")
    ap.add_argument("character")
    ap.add_argument("out")
    ap.add_argument("--titles", default=None)
    a = ap.parse_args()
    src, out = Path(a.scenes), Path(a.out)
    if not NAME.match(a.character):
        sys.exit(f"{a.character}: lowercase letters, digits, - and _")
    titles = json.loads(Path(a.titles).read_text()) if a.titles else {}
    out.mkdir(parents=True, exist_ok=True)
    catalogue = out / "scenes.json"
    scenes = json.loads(catalogue.read_text())["scenes"] if catalogue.exists() else []
    scenes = [s for s in scenes if s["character"] != a.character]
    for png in sorted(src.glob("*.png")):
        name = png.stem
        if name.endswith("-anim"):
            continue
        if not NAME.match(name):
            sys.exit(f"{png}: the name must be lowercase letters, digits, - and _")
        files = [f"{name}.png"]
        if (src / f"{name}.toml").exists():
            files += [f"{name}.toml", f"{name}-anim.png"]
        entry = {"character": a.character, "name": name, "title": titles.get(name, {}), "files": []}
        for f in files:
            data = (src / f).read_bytes()
            asset = f"{a.character}-{f}"
            shutil.copyfile(src / f, out / asset)
            entry["files"].append(
                {"file": f, "asset": asset, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
            )
        scenes.append(entry)
        print(f"{a.character}/{name}: {', '.join(files)}")
    catalogue.write_text(json.dumps({"format": 1, "scenes": scenes}, indent=1, ensure_ascii=False) + "\n")
    print(f"{catalogue}: {len(scenes)} scene(s)")


if __name__ == "__main__":
    main()
