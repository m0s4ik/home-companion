#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Turns a directory of animation frames (frame_000.png, frame_001.png, ...) into a character's
animation sheet (characters/README.md, "Animations") and prints its [animations.<name>] table.

usage: make-animation.py FRAMES_DIR POSE_PNG OUT_PNG [--name NAME] [--fps 16] [--pause 0]
                         [--part NAME:FIRST:END[:PAUSE]]... [--threshold 6] [--background "#03050d"]

--part declares one more animation played from frames FIRST..END-1 of the same sheet (Alfred's
loading clip contains the idle and wake-up clips, so the three share one sheet).

- Alignment: frame 0 is placed over POSE_PNG (the still pose the animation starts from) by
  matching the figure's bounding box, then refined by trying nearby scales and shifts (lowest mean
  difference), so the figure does not jump when the body switches between pose and animation.
- Box: only the part of the frames where the figure ever is (plus a margin) is kept; the rest is the
  flat background. Pixels close to the background are snapped to it (no flicker, better
  compression).
- Sheet: one RGBA PNG, the frames stacked vertically. Frame 0 is opaque (the key frame); in every
  other frame a pixel is opaque only where it differs from the previous frame *as the body
  reconstructs it* by more than THRESHOLD (any channel), transparent (and black) elsewhere: the
  body keeps the previous pixel. Consecutive frames differ in 0.3-2 % of the pixels, so the sheet
  is small and the body holds only the changed pixels in memory.

Needs Pillow and numpy (a tool for the build machine, not for the device).
"""
import argparse
import glob
import os
import sys

import numpy as np
from PIL import Image

FIGURE = 24  # a pixel belongs to the figure when a channel differs from the background by more
SNAP = 12  # pixels this close to the background become the background
MARGIN = 4


def parse_rgb(s):
    s = s.lstrip("#")
    return np.array([int(s[i:i + 2], 16) for i in (0, 2, 4)], dtype=np.int16)


def bbox(mask):
    ys, xs = np.nonzero(mask)
    if len(xs) == 0:
        return None
    return xs.min(), ys.min(), xs.max() + 1, ys.max() + 1


def align(frame0, pose, bg):
    """Scale and position (pose pixels) of frame 0 over the pose."""
    f = bbox((np.abs(frame0 - bg).max(axis=2) > FIGURE))
    p = bbox((np.abs(pose - bg).max(axis=2) > FIGURE))
    a0 = (p[3] - p[1]) / (f[3] - f[1])
    cx = (p[0] + p[2]) / 2 - (f[0] + f[2]) / 2 * a0
    cy = (p[1] + p[3]) / 2 - (f[1] + f[3]) / 2 * a0
    pose_l = pose.mean(axis=2)
    src = Image.fromarray(frame0.astype(np.uint8))
    best = None
    for d in range(-9, 10, 3):
        a = a0 * (1 + d / 1000)
        w, h = round(frame0.shape[1] * a), round(frame0.shape[0] * a)
        test = np.asarray(src.resize((w, h), Image.BILINEAR)).astype(np.int16).mean(axis=2)
        for dx in range(-3, 4):
            for dy in range(-3, 4):
                x, y = round(cx) + dx, round(cy) + dy
                x0, y0 = max(0, x), max(0, y)
                x1, y1 = min(pose.shape[1], x + w), min(pose.shape[0], y + h)
                if x1 - x0 < w // 2 or y1 - y0 < h // 2:
                    continue
                err = np.abs(pose_l[y0:y1, x0:x1] - test[y0 - y:y1 - y, x0 - x:x1 - x]).mean()
                if best is None or err < best[0]:
                    best = (err, w / frame0.shape[1], h / frame0.shape[0], x, y)
    return best


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("frames")
    ap.add_argument("pose")
    ap.add_argument("out")
    ap.add_argument("--name", default=None)
    ap.add_argument("--fps", type=float, default=16)
    ap.add_argument("--pause", type=float, default=0)
    ap.add_argument("--part", action="append", default=[])
    ap.add_argument("--threshold", type=int, default=6)
    ap.add_argument("--background", default="#03050d")
    a = ap.parse_args()

    bg = parse_rgb(a.background)
    files = sorted(glob.glob(os.path.join(a.frames, "frame_*.png")))
    if not files:
        sys.exit(f"make-animation: no frame_*.png in {a.frames}")
    frames = np.stack([np.asarray(Image.open(f).convert("RGB")).astype(np.int16) for f in files])
    pose = np.asarray(Image.open(a.pose).convert("RGB")).astype(np.int16)

    near = np.abs(frames - bg).max(axis=3) <= SNAP
    frames[near] = bg
    box = bbox((~near).any(axis=0))
    h, w = frames.shape[1:3]
    x0, y0 = max(0, box[0] - MARGIN), max(0, box[1] - MARGIN)
    x1, y1 = min(w, box[2] + MARGIN), min(h, box[3] + MARGIN)
    frames = frames[:, y0:y1, x0:x1]

    err, sx, sy, px, py = align(np.asarray(Image.open(files[0]).convert("RGB")).astype(np.int16), pose, bg)
    rect = [px + x0 * sx, py + y0 * sy, px + x1 * sx, py + y1 * sy]

    n, fh, fw = frames.shape[:3]
    sheet = np.zeros((n * fh, fw, 4), dtype=np.uint8)
    sheet[:fh, :, :3] = frames[0]
    sheet[:fh, :, 3] = 255
    shown = frames[0].copy()
    changed_total = 0
    for i in range(1, n):
        changed = np.abs(frames[i] - shown).max(axis=2) > a.threshold
        shown[changed] = frames[i][changed]
        part = sheet[i * fh:(i + 1) * fh]
        part[changed, :3] = frames[i][changed]
        part[changed, 3] = 255
        changed_total += int(changed.sum())
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    Image.fromarray(sheet, "RGBA").save(a.out, optimize=True)

    name = a.name or os.path.splitext(os.path.basename(a.out))[0]
    size = os.path.getsize(a.out)
    print(f"# {name}: {n} frames {fw}x{fh}, {changed_total / max(1, n - 1) / (fw * fh) * 100:.2f} % of the "
          f"pixels change per frame, {size / 1024:.0f} KiB; aligned on {os.path.basename(a.pose)} "
          f"(mean difference {err:.1f})", file=sys.stderr)
    parts = [(name, 0, n, a.pause)]
    for p in a.part:
        f = p.split(":")
        parts.append((f[0], int(f[1]), int(f[2]), float(f[3]) if len(f) > 3 else 0))
    for pname, first, end, pause in parts:
        print(f"\n[animations.{pname}]")
        print(f'file = "animations/{os.path.basename(a.out)}"')
        print(f"size = [{fw}, {fh}]")
        print(f"frames = [{first}, {end}]")
        print(f"fps = {a.fps:g}")
        if pause:
            print(f"pause = {pause:g}")
        print("rect = [" + ", ".join(f"{v:.1f}" for v in rect) + "]")

if __name__ == "__main__":
    main()
