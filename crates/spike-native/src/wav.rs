// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Reads a 16 kHz S16 WAV file (no resampling in this spike). Mono files are read as they are; a
//! multi-channel file (the device microphone captures 2 channels) needs an explicit channel.

pub const SAMPLE_RATE: u32 = 16_000;

pub fn read_16k(path: &str, channel: Option<usize>) -> Result<Vec<i16>, String> {
    let r = hound::WavReader::open(path).map_err(|e| format!("{path}: {e}"))?;
    let s = r.spec();
    if s.sample_rate != SAMPLE_RATE || s.bits_per_sample != 16 || s.sample_format != hound::SampleFormat::Int {
        return Err(format!(
            "{path}: need 16000 Hz S16, got {} Hz, {} bit {:?}",
            s.sample_rate, s.bits_per_sample, s.sample_format
        ));
    }
    let ch = s.channels as usize;
    let pick = match (ch, channel) {
        (1, None | Some(0)) => 0,
        (_, Some(c)) if c < ch => c,
        (_, Some(c)) => return Err(format!("{path}: has {ch} channel(s), no channel {c}")),
        (_, None) => return Err(format!("{path}: has {ch} channels; choose one with --channel N")),
    };
    let all: Vec<i16> = r.into_samples::<i16>().collect::<Result<_, _>>().map_err(|e| format!("{path}: {e}"))?;
    Ok(all.into_iter().skip(pick).step_by(ch).collect())
}
