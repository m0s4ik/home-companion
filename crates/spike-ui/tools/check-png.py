#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
#
# Checks a spike-ui screenshot: 800x1280 RGB, not uniform, and the box band
# (y 930..1110, where only the animated 64x64 box is drawn) contains about one
# box worth of pixels that differ from the background.
#
#   check-png.py SHOT.png
#   check-png.py SHOT.png --fb FAKE_FB --geometry 800x1280x3200
#       additionally compares the screenshot with a BGRX framebuffer dump
#       (pixel for pixel) and checks the box band in the framebuffer too.
#
# Pure python3 (zlib + struct). Exit status 0 = pass.
import argparse
import struct
import sys
import zlib

WIDTH, HEIGHT = 800, 1280
BAND = (930, 1110)
BOX = 64 * 64


def fail(msg):
    print(f"check-png: FAIL: {msg}")
    sys.exit(1)


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    return b if pb <= pc else c


def read_png(path):
    """Returns (width, height, channels, rows) for 8-bit RGB/RGBA PNGs."""
    try:
        with open(path, "rb") as f:
            data = f.read()
    except OSError as e:
        fail(str(e))
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        fail(f"{path}: not a PNG")
    pos, idat, ihdr = 8, b"", None
    while pos < len(data):
        (length,) = struct.unpack(">I", data[pos:pos + 4])
        kind = data[pos + 4:pos + 8]
        body = data[pos + 8:pos + 8 + length]
        pos += 12 + length
        if kind == b"IHDR":
            ihdr = struct.unpack(">IIBBBBB", body)
        elif kind == b"IDAT":
            idat += body
        elif kind == b"IEND":
            break
    width, height, depth, color, _, _, interlace = ihdr
    if depth != 8 or color not in (2, 6) or interlace:
        fail(f"{path}: expected 8-bit RGB or RGBA, non-interlaced (depth {depth}, color type {color})")
    ch = 3 if color == 2 else 4
    raw = zlib.decompress(idat)
    stride = width * ch
    rows, prev = [], bytearray(stride)
    for y in range(height):
        ft = raw[y * (stride + 1)]
        line = bytearray(raw[y * (stride + 1) + 1:(y + 1) * (stride + 1)])
        for i in range(stride):
            a = line[i - ch] if i >= ch else 0
            b = prev[i]
            c = prev[i - ch] if i >= ch else 0
            if ft == 1:
                line[i] = (line[i] + a) & 255
            elif ft == 2:
                line[i] = (line[i] + b) & 255
            elif ft == 3:
                line[i] = (line[i] + ((a + b) >> 1)) & 255
            elif ft == 4:
                line[i] = (line[i] + paeth(a, b, c)) & 255
        rows.append(bytes(line))
        prev = line
    return width, height, ch, rows


def pixel(rows, ch, x, y):
    i = x * ch
    return tuple(rows[y][i:i + 3])


def check_band(name, get):
    """`get(x, y)` -> (r, g, b). The box band must hold about one box."""
    bg = get(4, BAND[0])
    differing = 0
    for y in range(BAND[0], BAND[1]):
        for x in range(WIDTH):
            p = get(x, y)
            if sum(abs(p[k] - bg[k]) for k in range(3)) > 24:
                differing += 1
    if not 0.8 * BOX <= differing <= 2 * BOX:
        fail(f"{name}: box band has {differing} non-background pixels, expected about {BOX}")
    return differing


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("png")
    ap.add_argument("--fb")
    ap.add_argument("--geometry", default="800x1280x3200")
    a = ap.parse_args()

    w, h, ch, rows = read_png(a.png)
    if (w, h) != (WIDTH, HEIGHT):
        fail(f"size {w}x{h}, expected {WIDTH}x{HEIGHT}")
    if ch != 3:
        fail("expected an RGB PNG (no alpha)")
    if len(set(rows)) < 2 or len(set(rows[HEIGHT // 2])) < 2 * 3:
        fail("image is uniform")
    n = check_band("png", lambda x, y: pixel(rows, ch, x, y))
    print(f"check-png: {a.png}: {w}x{h} RGB, not uniform, box band {n} px: ok")

    if a.fb:
        fw, fh, line = (int(v) for v in a.geometry.split("x"))
        with open(a.fb, "rb") as f:
            fb = f.read()
        if (fw, fh) != (w, h) or len(fb) < fh * line:
            fail(f"framebuffer {a.fb}: geometry {a.geometry} does not match the screenshot or file size")

        def fbpix(x, y):
            o = y * line + x * 4
            return (fb[o + 2], fb[o + 1], fb[o])  # B, G, R, X in memory

        mismatches = 0
        for y in range(fh):
            for x in range(fw):
                if fbpix(x, y) != pixel(rows, ch, x, y):
                    mismatches += 1
        if mismatches:
            fail(f"framebuffer {a.fb} differs from the screenshot in {mismatches} pixels")
        n = check_band("framebuffer", fbpix)
        print(f"check-png: {a.fb}: BGRX framebuffer equals the screenshot, box band {n} px: ok")


if __name__ == "__main__":
    main()
