#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Writes two placeholder character poses, fixtures/pose-a.png (smiling) and
# fixtures/pose-b.png (mouth open), 600x600 RGBA, transparent outside the face.
# The images themselves are dedicated to the public domain (CC0 1.0).
# Pure python3 (zlib + struct), no third-party modules.
import os
import struct
import sys
import zlib

SIZE = 600
SS = 2  # supersampling per axis for smooth edges

TRANSPARENT = (0, 0, 0, 0)
FACE = (246, 196, 70, 255)
OUTLINE = (120, 80, 20, 255)
EYE = (40, 30, 30, 255)
SHINE = (255, 255, 255, 255)
MOUTH = (150, 40, 40, 255)
CHEEK = (240, 140, 110, 255)


def inside(x, y, cx, cy, rx, ry=None):
    ry = rx if ry is None else ry
    dx = (x - cx) / rx
    dy = (y - cy) / ry
    return dx * dx + dy * dy <= 1.0


def color_at(x, y, mouth_open):
    """Colour of the sub-sample at (x, y) in 600x600 coordinates."""
    if not inside(x, y, 300, 300, 290):
        return TRANSPARENT
    if not inside(x, y, 300, 300, 278):
        return OUTLINE
    for ex in (205, 395):
        if inside(x, y, ex - 12, 225, 14):
            return SHINE
        if inside(x, y, ex, 240, 34, 46):
            return EYE
    for cx in (150, 450):
        if inside(x, y, cx, 345, 40, 26):
            return CHEEK
    if mouth_open:
        if inside(x, y, 300, 420, 70, 62):
            return MOUTH
    else:
        # A smile: the lower part of a ring.
        if y > 360 and inside(x, y, 300, 340, 150, 110) and not inside(x, y, 300, 320, 138, 104):
            return MOUTH
    return FACE


def render(mouth_open):
    rows = []
    n = SS * SS
    for py in range(SIZE):
        row = bytearray([0])  # filter type None
        for px in range(SIZE):
            acc = [0, 0, 0, 0]
            for sy in range(SS):
                for sx in range(SS):
                    c = color_at(px + (sx + 0.5) / SS, py + (sy + 0.5) / SS, mouth_open)
                    # premultiply so transparent samples do not darken edges
                    acc[0] += c[0] * c[3]
                    acc[1] += c[1] * c[3]
                    acc[2] += c[2] * c[3]
                    acc[3] += c[3]
            a = acc[3]
            if a == 0:
                row += b"\0\0\0\0"
            else:
                row += bytes((acc[0] // a, acc[1] // a, acc[2] // a, a // n))
        rows.append(bytes(row))
    return b"".join(rows)


def chunk(kind, data):
    body = kind + data
    return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)


def write_png(path, raw):
    ihdr = struct.pack(">IIBBBBB", SIZE, SIZE, 8, 6, 0, 0, 0)  # 8-bit RGBA
    with open(path, "wb") as f:
        f.write(b"\x89PNG\r\n\x1a\n")
        f.write(chunk(b"IHDR", ihdr))
        f.write(chunk(b"IDAT", zlib.compress(raw, 9)))
        f.write(chunk(b"IEND", b""))


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "fixtures")
    os.makedirs(out, exist_ok=True)
    for name, mouth_open in (("pose-a.png", False), ("pose-b.png", True)):
        path = os.path.join(out, name)
        write_png(path, render(mouth_open))
        print(f"{path}: {os.path.getsize(path)} bytes")


if __name__ == "__main__":
    main()
