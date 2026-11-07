#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Checks a spike-gpu screenshot (build machine; needs Pillow):
# - 800x1280;
# - the character box shows one of the animation frames (scaled to the box):
#   the best-matching frame's mean absolute difference must be small;
# - everything outside the box and the clock is the background #03050D;
# - the clock band has bright text pixels;
# - with --fb, a BGRX framebuffer dump is pixel-identical to the screenshot.
#
#   check-shot.py SHOT.png --frames-dir DIR --poses idle,listening --box X,Y,W,H [--max-diff 4]
#                 [--fb FILE --geometry 800x1280x3200]
import argparse
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageStat

BG = (0x03, 0x05, 0x0D)
CLOCK = (1050, 1250)


def fail(msg):
    print(f"check-shot: FAIL: {msg}")
    sys.exit(1)


def mean_diff(a, b):
    return sum(ImageStat.Stat(ImageChops.difference(a, b)).mean) / 3


def main():
    p = argparse.ArgumentParser()
    p.add_argument("shot")
    p.add_argument("--frames-dir", required=True)
    p.add_argument("--poses", default="idle,listening")
    p.add_argument("--box", required=True)
    p.add_argument("--max-diff", type=float, default=4.0)
    p.add_argument("--fb")
    p.add_argument("--geometry", default="800x1280x3200")
    a = p.parse_args()

    shot = Image.open(a.shot).convert("RGB")
    if shot.size != (800, 1280):
        fail(f"size {shot.size}, expected 800x1280")
    x, y, w, h = (int(v) for v in a.box.split(","))
    crop = shot.crop((x, y, x + w, y + h))

    best = (1e9, None)
    for pose in a.poses.split(","):
        for f in sorted(Path(a.frames_dir, pose).glob("frame_*.png")):
            frame = Image.open(f).convert("RGB").resize((w, h), Image.BILINEAR)
            d = mean_diff(crop, frame)
            if d < best[0]:
                best = (d, f"{pose}/{f.name}")
    print(f"check-shot: character box {w}x{h}@{x},{y}: best frame {best[1]}, mean abs diff {best[0]:.2f}")
    if best[0] > a.max_diff:
        fail(f"character box does not match any frame (best {best[0]:.2f} > {a.max_diff})")

    # Background: rows above the box and between the box and the clock, columns beside the box.
    bands = [(0, 0, 800, y), (0, y + h, 800, CLOCK[0]), (0, y, x, y + h), (x + w, y, 800, y + h), (0, CLOCK[1], 800, 1280)]
    for band in bands:
        if band[2] <= band[0] or band[3] <= band[1]:
            continue
        region = shot.crop(band)
        lo, hi = zip(*region.getextrema())
        if any(abs(l - c) > 1 or abs(hh - c) > 1 for l, hh, c in zip(lo, hi, BG)):
            fail(f"background band {band} is not {BG}: min {lo} max {hi}")
    clock = shot.crop((0, CLOCK[0], 800, CLOCK[1])).convert("L")
    bright = sum(clock.histogram()[200:])
    print(f"check-shot: clock band bright pixels {bright}")
    if bright < 5000:
        fail("no clock text in the clock band")

    if a.fb:
        gw, gh, line = (int(v) for v in a.geometry.split("x"))
        data = Path(a.fb).read_bytes()
        rows = b"".join(data[r * line:r * line + gw * 4] for r in range(gh))
        fbimg = Image.frombytes("RGB", (gw, gh), rows, "raw", "BGRX")
        d = ImageChops.difference(fbimg, shot).getbbox()
        if d is not None:
            fail(f"framebuffer differs from the screenshot in {d}")
        print("check-shot: framebuffer identical to the screenshot")
    print("check-shot: pass")


main()
