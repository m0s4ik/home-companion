#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Synthesizes a character's sounds into <dir>/*.wav (48 kHz, 16-bit mono).

Soft, short sounds made of a few partials with a gentle attack and an exponential decay
(marimba- and bell-like), a little low-pass and a short room tail, all at the same peak level.
The body plays <character>/sounds/<name>.wav when the file is there and its own plain tones
otherwise, so any file can be replaced by hand (same name, any rate, mono or stereo).

    tools/make-sounds.py characters/alfred/sounds

Only the standard library is used.
"""

import math
import struct
import sys
import wave
from pathlib import Path

RATE = 48_000
PEAK = 0.35  # about -9 dBFS before the volume: softer than the old -6 dBFS tones


def note(freq, dur, kind="marimba", vel=1.0):
    """One struck note: a list of samples."""
    n = int(RATE * dur)
    if kind == "marimba":
        # Wooden bar: fundamental, a 4th harmonic that dies fast, a faint 10th.
        partials = [(1.0, 1.0, 3.2), (3.98, 0.22, 14.0), (9.8, 0.05, 30.0)]
        attack = 0.004
    elif kind == "bell":
        # Small soft bell: inharmonic partials, long gentle decay.
        partials = [(1.0, 1.0, 2.2), (2.0, 0.35, 3.0), (2.76, 0.18, 4.5), (5.4, 0.05, 9.0)]
        attack = 0.006
    elif kind == "glass":
        # Glass / celesta: almost pure, round.
        partials = [(1.0, 1.0, 3.0), (2.0, 0.12, 6.0), (3.0, 0.04, 10.0)]
        attack = 0.008
    elif kind == "wood":
        # A soft wooden tick.
        partials = [(1.0, 1.0, 38.0), (2.4, 0.4, 60.0)]
        attack = 0.002
    else:
        raise ValueError(kind)
    out = []
    for i in range(n):
        t = i / RATE
        env_a = min(1.0, t / attack)
        s = 0.0
        for ratio, amp, decay in partials:
            s += amp * math.exp(-decay * t) * math.sin(2 * math.pi * freq * ratio * t)
        out.append(vel * env_a * s)
    return out


def soft_bubble():
    """A control pressed: a soft bubble, a short sine whose pitch rises quickly (300 -> 800 Hz)
    and dies in ~60 ms. Chosen by Andrea from eight variants (2026-11-06)."""
    out, phase = [], 0.0
    for i in range(int(RATE * 0.06)):
        t = i / RATE
        f = 300 + 500 * (1 - math.exp(-90 * t))
        phase += 2 * math.pi * f / RATE
        out.append(math.sin(phase) * min(1.0, t / 0.002) * math.exp(-70 * t))
    return out


def falling_bubble(f0, f1, vel):
    """A soft bubble whose pitch falls from f0 to f1 Hz, ~80 ms."""
    out, phase = [], 0.0
    for i in range(int(RATE * 0.08)):
        t = i / RATE
        f = f1 + (f0 - f1) * math.exp(-45 * t)
        phase += 2 * math.pi * f / RATE
        out.append(vel * math.sin(phase) * min(1.0, t / 0.002) * math.exp(-55 * t))
    return out


def mix(events, length=None):
    """events: (start_seconds, samples). Returns the sum."""
    end = max(int(st * RATE) + len(x) for st, x in events)
    if length:
        end = max(end, int(length * RATE))
    buf = [0.0] * end
    for st, x in events:
        o = int(st * RATE)
        for i, v in enumerate(x):
            buf[o + i] += v
    return buf


def soften(buf):
    """One-pole low-pass (~5 kHz) and a short, quiet room tail (two feedback delays)."""
    a = math.exp(-2 * math.pi * 5000 / RATE)
    y, out = 0.0, []
    for v in buf:
        y = (1 - a) * v + a * y
        out.append(y)
    tail = int(RATE * 0.25)
    out += [0.0] * tail
    for d_ms, g in ((31, 0.18), (47, 0.12)):
        d = int(RATE * d_ms / 1000)
        for i in range(d, len(out)):
            out[i] += g * out[i - d]
    return out


GAIN = {"tap": 0.8, "home": 0.7, "reminder": 1.25}  # relative to PEAK


def finish(buf, name=""):
    peak = max(1e-9, max(abs(v) for v in buf))
    buf = [v * PEAK * GAIN.get(name, 1.0) / peak for v in buf]
    # 5 ms fade in, 30 ms fade out, trailing silence trimmed to the tail's end
    fi, fo = int(RATE * 0.005), int(RATE * 0.03)
    for i in range(min(fi, len(buf))):
        buf[i] *= i / fi
    while len(buf) > fo and abs(buf[-1]) < 1e-4:
        buf.pop()
    for i in range(min(fo, len(buf))):
        buf[-1 - i] *= i / fo
    return buf


def write(path, buf):
    with wave.open(str(path), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(b"".join(struct.pack("<h", int(max(-1, min(1, v)) * 32767)) for v in buf))


# Pitches (Hz)
C4, D4, F4, G4, A4 = 261.63, 293.66, 349.23, 392.0, 440.0
C5, D5, E5, F5, G5, A5, B5 = 523.25, 587.33, 659.25, 698.46, 783.99, 880.0, 987.77
C6, E6, G6 = 1046.5, 1318.5, 1568.0

SOUNDS = {
    # "I'm listening": two rising soft notes.
    "wake": lambda: mix([(0.0, note(E5, 0.35)), (0.09, note(A5, 0.5))]),
    # Something went wrong / Home Assistant does not answer: two falling notes, a minor third.
    "error": lambda: mix([(0.0, note(A4, 0.4, vel=0.9)), (0.14, note(F4, 0.6, vel=0.9))]),
    # Not understood and nobody else to ask (no reflex, no Home Assistant): two low muted taps.
    "not-understood": lambda: mix([(0.0, note(D4, 0.18, vel=0.8)), (0.16, note(D4, 0.25, vel=0.7))]),
    # A volume step: one small glass tick (played at the new level).
    "volume": lambda: note(A5, 0.18, "glass", 0.8),
    # Timer started: two wooden ticks, then a small bell.
    "timer-set": lambda: mix([(0.0, note(1800, 0.06, "wood", 0.5)), (0.11, note(1800, 0.06, "wood", 0.5)),
                              (0.24, note(E6, 0.9, "bell", 0.8))]),
    # Alarm set: two long rising bell notes, a fifth.
    "alarm-set": lambda: mix([(0.0, note(C5, 0.9, "bell")), (0.22, note(G5, 1.1, "bell"))]),
    # Timer or alarm removed: three falling marimba notes.
    "cancelled": lambda: mix([(0.0, note(G5, 0.3)), (0.11, note(E5, 0.3)), (0.22, note(C5, 0.5))]),
    # The time: two soft clock chimes.
    "time": lambda: mix([(0.0, note(G5, 0.9, "bell", 0.85)), (0.32, note(C6, 1.2, "bell", 0.85))]),
    # Done, nothing else to say (how long is left, a reflex with only a screen answer).
    "done": lambda: mix([(0.0, note(C5, 0.3)), (0.08, note(E5, 0.3)), (0.16, note(G5, 0.5))]),
    # Something Alfred managed to do (linked to Home Assistant): the classic "ok", a quick rising
    # arpeggio landing on a small bell an octave up.
    "success": lambda: mix([(0.0, note(C5, 0.25)), (0.07, note(E5, 0.25)), (0.14, note(G5, 0.3)),
                            (0.22, note(C6, 1.0, "bell", 0.85))]),
    # "Silenzio": one low, short, soft note.
    "silence": lambda: note(C4, 0.4, "glass", 0.8),
    # A reminder or a message card: three bright bell strokes rising, a clear "attention" (chosen
    # by Andrea among four, 2026-11-06); a little louder than the others (GAIN).
    "reminder": lambda: mix([(0.0, note(C6, 0.9, "bell")), (0.22, note(E6, 0.9, "bell")),
                             (0.44, note(G6, 1.4, "bell"))]),
    # Back home from Home Assistant's view: the tap's bubble turned downwards, twice, the second
    # lower and softer (the same family as the tap, so they sit together).
    "home": lambda: mix([(0.0, falling_bubble(700, 400, 1.0)), (0.08, falling_bubble(520, 300, 0.6))]),
    # A control pressed: a soft bubble.
    "tap": lambda: soft_bubble(),
    # One ring of the timer bell (the body repeats it with a pause): soft but clear.
    "bell": lambda: mix([(0.0, note(E6, 0.7, "bell")), (0.18, note(C6, 0.7, "bell")),
                         (0.36, note(G5, 1.0, "bell"))]),
}


def main():
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[0], file=sys.stderr)
        print("usage: make-sounds.py DIR", file=sys.stderr)
        sys.exit(2)
    out = Path(sys.argv[1])
    out.mkdir(parents=True, exist_ok=True)
    for name, make in SOUNDS.items():
        raw = make()
        # The bubbles stay dry (no room, no low-pass).
        buf = finish(raw if name in ("tap", "home") else soften(raw), name)
        write(out / f"{name}.wav", buf)
        print(f"{name}.wav  {len(buf) / RATE:.2f} s")


if __name__ == "__main__":
    main()
