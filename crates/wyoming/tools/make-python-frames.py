# SPDX-License-Identifier: GPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 Andrea Mattera
"""Writes fixtures/python-frames.bin with the real wyoming package (1.11.0).

Run once, in a throw-away venv, from the crate directory:
    python3 -m venv /tmp/wy && /tmp/wy/bin/pip install wyoming==1.11.0
    /tmp/wy/bin/python -I tools/make-python-frames.py fixtures/info-expected.json fixtures/python-frames.bin
tests/python_compat.rs parses the frames and compares our header lines with these bytes.
"""

import io
import json
import sys

from importlib.metadata import version

from wyoming.asr import Transcribe, Transcript, TranscriptChunk
from wyoming.audio import AudioChunk, AudioStart, AudioStop
from wyoming.error import Error
from wyoming.event import Event, write_event
from wyoming.info import Describe, Info
from wyoming.ping import Ping, Pong
from wyoming.pipeline import PipelineStage, RunPipeline
from wyoming.satellite import PauseSatellite, RunSatellite
from wyoming.snd import Played
from wyoming.timer import TimerCancelled, TimerFinished, TimerStarted, TimerUpdated
from wyoming.tts import Synthesize, SynthesizeVoice
from wyoming.vad import VoiceStarted, VoiceStopped
from wyoming.wake import Detection

assert version("wyoming") == "1.11.0", version("wyoming")

info = Info.from_event(Event("info", json.load(open(sys.argv[1], encoding="utf-8"))))
events = [
    Describe().event(),
    info.event(),
    Ping(text="x").event(),
    Pong().event(),
    RunSatellite().event(),
    Detection(name="okay_alfred", timestamp=1234).event(),
    RunPipeline(start_stage=PipelineStage.ASR, end_stage=PipelineStage.TTS).event(),
    AudioStart(rate=16000, width=2, channels=1, timestamp=0).event(),
    AudioChunk(rate=16000, width=2, channels=1, audio=bytes(i % 256 for i in range(320)), timestamp=20).event(),
    AudioStop(timestamp=40).event(),
    Transcribe(language="en").event(),
    VoiceStarted(timestamp=300).event(),
    VoiceStopped(timestamp=1500).event(),
    Transcript(text="Che ore sono? È l'una").event(),
    Synthesize(text="It is ten past two.", voice=SynthesizeVoice(name="en_US-lessac-medium", language="en")).event(),
    AudioStart(rate=22050, width=2, channels=1, timestamp=0).event(),
    AudioChunk(rate=22050, width=2, channels=1, audio=bytes(2048), timestamp=0).event(),
    AudioStop(timestamp=46).event(),
    Played().event(),
    Error(text="No text recognized", code="stt-no-text-recognized").event(),
    TimerStarted(id="t1", total_seconds=90, name="Pasta", start_minutes=1, start_seconds=30).event(),
    TimerUpdated(id="t1", is_active=True, total_seconds=88).event(),
    TimerCancelled(id="t1").event(),
    TimerFinished(id="t1").event(),
    PauseSatellite().event(),
    TranscriptChunk(text="che").event(),
]
buf = io.BytesIO()
for e in events:
    write_event(e, buf)
with open(sys.argv[2], "wb") as f:
    f.write(buf.getvalue())
