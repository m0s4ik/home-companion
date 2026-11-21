#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Makes the browser panel's pictures from Alfred's poses (characters/alfred/poses, CC0): small
WebP with transparency for the page, PNG for the icons. The output is in the repo
(crates/panel/web/art, served from the binary); run again only when the poses change.

    python3 crates/panel/tools/make-art.py
"""
from pathlib import Path

from PIL import Image, ImageDraw

ROOT = Path(__file__).resolve().parents[3]
POSES = ROOT / "characters/alfred/poses"
OUT = ROOT / "crates/panel/web/art"
BG = (3, 5, 13)  # the poses' background, the screen's `bg`
CROP = (212, 0, 812, 1024)  # where the figure stands (character.toml `crop`)
FACE = (400, 28, 610, 238)  # the face in the 1024 x 1024 poses


def cut_out(img):
    """The background made transparent, with a soft edge (the poses are drawn on `bg`)."""
    img = img.convert("RGB")
    alpha = img.point(lambda v: v).convert("L")
    px, a = img.load(), alpha.load()
    for y in range(img.height):
        for x in range(img.width):
            r, g, b = px[x, y]
            d = max(abs(r - BG[0]), abs(g - BG[1]), abs(b - BG[2]))
            a[x, y] = 0 if d < 6 else 255 if d > 22 else (d - 6) * 255 // 16
    out = img.convert("RGBA")
    out.putalpha(alpha)
    return out


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    poses = {
        "idle": 480,
        "sleeping": 300,
        "thinking": 300,
        "listening": 300,
        "error": 300,
    }
    for name, h in poses.items():
        img = cut_out(Image.open(POSES / f"{name}.png").crop(CROP))
        w = round(img.width * h / img.height)
        img.resize((w, h), Image.LANCZOS).save(OUT / f"{name}.webp", "WEBP", quality=82, method=6)
    face = Image.open(POSES / "idle.png").convert("RGB").crop(FACE)
    # The portrait: round, on the page's background.
    big = face.resize((192, 192), Image.LANCZOS)
    mask = Image.new("L", (192, 192), 0)
    ImageDraw.Draw(mask).ellipse((0, 0, 191, 191), fill=255)
    round_face = Image.new("RGBA", (192, 192), (0, 0, 0, 0))
    round_face.paste(big, (0, 0), mask)
    round_face.resize((96, 96), Image.LANCZOS).save(OUT / "face.webp", "WEBP", quality=85, method=6)
    # The icons: square, opaque (iOS draws its own corners).
    for size, name in [(32, "favicon-32.png"), (180, "apple-touch-icon.png")]:
        icon = face.resize((size, size), Image.LANCZOS).quantize(colors=128 if size > 64 else 64)
        icon.save(OUT / name, optimize=True)
    total = sum(p.stat().st_size for p in OUT.iterdir())
    for p in sorted(OUT.iterdir()):
        print(f"{p.name:24} {p.stat().st_size:7}")
    print(f"{'total':24} {total:7}")


if __name__ == "__main__":
    main()
