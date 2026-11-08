<!-- SPDX-License-Identifier: CC0-1.0 -->
<!-- SPDX-FileCopyrightText: 2026 Andrea Mattera -->

# Alfred's sounds

Short, soft sounds made by `tools/make-sounds.py` (struck notes: marimba, small bell, glass, a wooden tick; a gentle
attack, a natural decay, a little room). Every file can be replaced by hand with any WAV of the same name (any rate,
mono or stereo); a missing file falls back to a plain tone in the body. Regenerate them all with
`tools/make-sounds.py characters/alfred/sounds`.

| File | When |
|---|---|
| `wake.wav` | "Okay Alfred" heard, or Parla: listening |
| `error.wav` | something went wrong, or Home Assistant does not answer |
| `not-understood.wav` | not a reflex and no Home Assistant to ask, or nothing said after the wake word |
| `time.wav` | reflex "che ore sono" (the time is on the screen) |
| `timer-set.wav` | reflex: a timer started |
| `alarm-set.wav` | reflex: an alarm set |
| `cancelled.wav` | reflex: a timer or an alarm removed |
| `done.wav` | reflex answered on the screen only (how long is left, which timers, do not disturb) |
| `success.wav` | something the user waited for worked (linked to Home Assistant, a house command went through): the classic "ok"; a short answer after the command is then not spoken |
| `silence.wav` | reflex "silenzio", "lascia stare", "spegni lo schermo", "buonanotte" |
| `volume.wav` | a volume step, at the new level |
| `bell.wav` | one ring of a finished timer or alarm (repeated with a pause) |
| `reminder.wav` | a reminder or message card appears (by day only); a due reminder repeats it until OK |
| `home.wav` | back home from Home Assistant's view by the user (home button, swipe, Vol+ and Vol−) |
| `tick.wav` | each of a timer's last ten seconds (very soft; not over anything else, not with do not disturb or at night) |
| `tap.wav` | a control on the screen pressed (very soft; not over an answer or the bell) |

So it is clear who understood: a reflex done by Alfred himself has its own sound and answers on the screen; Home
Assistant answers aloud.
