#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Compares screenshots with the golden references, pixel by pixel (PNG decoded with the
standard library only: 8-bit RGB/RGBA, not interlaced, as home-companion and Slint write them).

usage: check-golden.py GOLDEN_DIR NEW_DIR [STATE...]

Every GOLDEN_DIR/<state>.png must exist in NEW_DIR with identical pixels. On a difference it
prints how many pixels differ and their bounding box. Regenerate the references with
tools/update-golden.sh when companion.slint, a character or a locale changes on purpose.
"""
import os
import struct
import sys
import zlib

STATES = ["home", "cards", "listening", "thinking", "speaking", "error", "offline", "ringing", "volume", "altro", "chat", "timers", "settings", "language", "screen", "music", "setup", "night", "starting", "suspend", "new-timer", "new-alarm"]


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def decode(path):
    with open(path, "rb") as f:
        data = f.read()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path}: not a PNG")
    pos, idat = 8, b""
    width = height = bpp = None
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", body)
            if depth != 8 or color not in (2, 6) or interlace:
                raise ValueError(f"{path}: only 8-bit RGB/RGBA non-interlaced PNGs")
            bpp = 3 if color == 2 else 4
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    raw = zlib.decompress(idat)
    stride = width * bpp
    out = bytearray(height * stride)
    prev = bytearray(stride)
    p = 0
    for y in range(height):
        ft = raw[p]
        line = bytearray(raw[p + 1:p + 1 + stride])
        p += 1 + stride
        if ft == 1:
            for i in range(bpp, stride):
                line[i] = (line[i] + line[i - bpp]) & 0xFF
        elif ft == 2:
            for i in range(stride):
                line[i] = (line[i] + prev[i]) & 0xFF
        elif ft == 3:
            for i in range(stride):
                left = line[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + ((left + prev[i]) >> 1)) & 0xFF
        elif ft == 4:
            for i in range(stride):
                left = line[i - bpp] if i >= bpp else 0
                upleft = prev[i - bpp] if i >= bpp else 0
                line[i] = (line[i] + paeth(left, prev[i], upleft)) & 0xFF
        out[y * stride:(y + 1) * stride] = line
        prev = line
    if bpp == 3:  # to RGBA
        rgba = bytearray(width * height * 4)
        rgba[0::4], rgba[1::4], rgba[2::4] = out[0::3], out[1::3], out[2::3]
        rgba[3::4] = b"\xff" * (width * height)
        out = rgba
    return width, height, bytes(out)


def compare(golden, new):
    gw, gh, gp = decode(golden)
    nw, nh, np = decode(new)
    if (gw, gh) != (nw, nh):
        return f"size {nw}x{nh}, golden {gw}x{gh}"
    if gp == np:
        return None
    n, x0, y0, x1, y1 = 0, gw, gh, -1, -1
    for i in range(0, len(gp), 4):
        if gp[i:i + 4] != np[i:i + 4]:
            n += 1
            x, y = (i // 4) % gw, (i // 4) // gw
            x0, y0, x1, y1 = min(x0, x), min(y0, y), max(x1, x), max(y1, y)
    return f"{n} pixels differ in x {x0}-{x1}, y {y0}-{y1}"


def main():
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    golden_dir, new_dir = sys.argv[1], sys.argv[2]
    states = sys.argv[3:] or STATES
    bad = 0
    for s in states:
        g, n = os.path.join(golden_dir, f"{s}.png"), os.path.join(new_dir, f"{s}.png")
        if not os.path.exists(g) or not os.path.exists(n):
            print(f"FAIL {s}: missing {g if not os.path.exists(g) else n}")
            bad += 1
            continue
        diff = compare(g, n)
        print(f"{'ok  ' if diff is None else 'FAIL'} {s}{'' if diff is None else ': ' + diff}")
        bad += diff is not None
    print("PASS" if not bad else f"FAIL ({bad})")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
