// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The wake word thread: consumes the capture frames. In `Wake` mode it runs the detector
//! and reports detections; in `Stream` mode it bypasses the detector and forwards the frames
//! to the core (which writes them to Home Assistant); in `Off` mode it drops them.

use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use audio::CaptureFrame;

use crate::core::{CoreEvent, MicControl, MicMode};

/// Frames of audio from before a detection sent with the utterance (8 x 20 ms).
const PRE_ROLL_FRAMES: usize = 8;

pub fn spawn(
    frames: Receiver<CaptureFrame>,
    mut detector: Option<wakeword::Detector>,
    mut stop_detectors: Vec<wakeword::Detector>,
    mic: Arc<MicControl>,
    core: Sender<CoreEvent>,
    stats: Arc<Mutex<Option<wakeword::Stats>>>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("wakeword".into())
        .spawn(move || {
            let mut last_stats = Instant::now();
            let mut stop_was_on = false;
            // The last frames heard by the detector: sent after a detection, so the phrase keeps
            // a first word said without a pause (the detector fires a little after "Alfred").
            let mut recent: VecDeque<CaptureFrame> = VecDeque::with_capacity(PRE_ROLL_FRAMES + 1);
            // Ends when the audio subsystem stops (its sender is dropped).
            while let Ok(frame) = frames.recv() {
                if mic.take_reset() {
                    if let Some(d) = &mut detector {
                        d.reset();
                    }
                }
                // The stop words: only while the core asks for them, from a clean state each time;
                // any of them stops.
                let stop_on = mic.stop_listening() && mic.mode() == MicMode::Wake;
                let mut gone = false;
                if stop_on {
                    for s in &mut stop_detectors {
                        if !stop_was_on {
                            s.reset();
                        }
                        match s.feed(&frame.samples) {
                            Ok(Some(det)) => {
                                if core.send(CoreEvent::StopWord(det)).is_err() {
                                    gone = true;
                                }
                            }
                            Ok(None) => {}
                            Err(e) => log::warn!("stop word: {e}"),
                        }
                    }
                }
                if gone {
                    break;
                }
                stop_was_on = stop_on;
                match mic.mode() {
                    MicMode::Wake => {
                        let Some(d) = &mut detector else { continue };
                        let result = d.feed(&frame.samples);
                        recent.push_back(frame);
                        if recent.len() > PRE_ROLL_FRAMES {
                            recent.pop_front();
                        }
                        match result {
                            Ok(Some(det)) => {
                                // Forward the very next frame: people often go on speaking right
                                // after "Okay Alfred", and the frames until the core has started
                                // the conversation used to go to the detector and be lost (the
                                // first word of "che ore sono"). The core puts the mode back to
                                // Wake if it does not start one.
                                mic.set(MicMode::Stream);
                                if core.send(CoreEvent::WakeWord(det)).is_err() {
                                    break;
                                }
                                let mut gone = false;
                                for f in recent.drain(..) {
                                    if core.send(CoreEvent::MicFrame(f)).is_err() {
                                        gone = true;
                                        break;
                                    }
                                }
                                if gone {
                                    break;
                                }
                            }
                            Ok(None) => {}
                            Err(e) => log::warn!("wake word: {e}"),
                        }
                    }
                    MicMode::Stream => {
                        recent.clear();
                        if core.send(CoreEvent::MicFrame(frame)).is_err() {
                            break;
                        }
                    }
                    MicMode::Off => recent.clear(),
                }
                if last_stats.elapsed() >= Duration::from_secs(1) {
                    last_stats = Instant::now();
                    if let (Some(d), Ok(mut s)) = (&detector, stats.lock()) {
                        *s = Some(d.stats());
                    }
                }
            }
            if let (Some(d), Ok(mut s)) = (&detector, stats.lock()) {
                *s = Some(d.stats());
            }
            log::debug!("wake word thread stopped");
        })
}
