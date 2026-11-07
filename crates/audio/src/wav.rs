// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Small WAV helpers (16-bit PCM only).

use crate::{CAPTURE_RATE, ChannelPick, Error};
use std::path::Path;

fn bad(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Unsupported(format!("{}: {e}", path.display()))
}

/// Reads a 16-bit integer WAV: (interleaved samples, rate, channels).
pub(crate) fn read_interleaved(path: &Path) -> Result<(Vec<i16>, u32, usize), Error> {
    let mut r = hound::WavReader::open(path).map_err(|e| match e {
        hound::Error::IoError(io) => Error::Io(io),
        e => bad(path, e),
    })?;
    let spec = r.spec();
    if spec.bits_per_sample != 16 || spec.sample_format != hound::SampleFormat::Int {
        return Err(bad(path, "only 16-bit integer PCM WAV is supported"));
    }
    if spec.channels == 0 {
        return Err(bad(path, "no channels"));
    }
    let samples: Vec<i16> = r
        .samples::<i16>()
        .collect::<Result<_, _>>()
        .map_err(|e| bad(path, e))?;
    Ok((samples, spec.sample_rate, spec.channels as usize))
}

/// Picks one channel (`Left` = first, `Right` = second, or the first of a mono file) or
/// averages the first two.
pub(crate) fn pick(interleaved: &[i16], channels: usize, pick: ChannelPick) -> Vec<i16> {
    let channels = channels.max(1);
    interleaved
        .chunks_exact(channels)
        .map(|f| pick_frame(f, pick))
        .collect()
}

#[inline]
pub(crate) fn pick_frame(f: &[i16], pick: ChannelPick) -> i16 {
    match (f.len(), pick) {
        (1, _) | (_, ChannelPick::Left) => f[0],
        (_, ChannelPick::Right) => f[1],
        (_, ChannelPick::Average) => ((f[0] as i32 + f[1] as i32) >> 1) as i16,
    }
}

/// Reads a 16-bit WAV as mono (channel chosen with `pick`) with its rate.
pub(crate) fn read_mono(path: &Path, p: ChannelPick) -> Result<(Vec<i16>, u32), Error> {
    let (s, rate, ch) = read_interleaved(path)?;
    Ok((pick(&s, ch, p), rate))
}

/// Reads a 16-bit WAV as 16 kHz mono (resampled with a sinc filter when it is not 16 kHz).
pub fn read_mono_16k(path: &Path, p: ChannelPick) -> Result<Vec<i16>, Error> {
    let (mono, rate) = read_mono(path, p)?;
    Ok(crate::resample::resample_clip(&mono, rate, CAPTURE_RATE))
}

/// Writes a mono 16-bit WAV.
pub fn write_mono(path: &Path, rate: u32, s: &[i16]) -> Result<(), Error> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(|e| bad(path, e))?;
    {
        let mut w16 = w.get_i16_writer(s.len() as u32);
        for &x in s {
            w16.write_sample(x);
        }
        w16.flush().map_err(|e| bad(path, e))?;
    }
    w.finalize().map_err(|e| bad(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks() {
        let s = [100i16, -100, 300, 100];
        assert_eq!(pick(&s, 2, ChannelPick::Left), vec![100, 300]);
        assert_eq!(pick(&s, 2, ChannelPick::Right), vec![-100, 100]);
        assert_eq!(pick(&s, 2, ChannelPick::Average), vec![0, 200]);
        assert_eq!(pick(&s, 1, ChannelPick::Right), s.to_vec());
        assert_eq!(
            pick_frame(&[i16::MAX, i16::MAX], ChannelPick::Average),
            i16::MAX
        );
        assert_eq!(
            pick_frame(&[i16::MIN, i16::MIN], ChannelPick::Average),
            i16::MIN
        );
    }

    #[test]
    fn round_trip_and_errors() {
        let dir = std::env::temp_dir().join(format!("audio-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("a.wav");
        let data: Vec<i16> = (0..1600).map(|i| (i * 7 % 300) as i16).collect();
        write_mono(&p, 16000, &data).unwrap();
        assert_eq!(read_mono_16k(&p, ChannelPick::Left).unwrap(), data);
        std::fs::write(dir.join("bad.wav"), b"not a wav").unwrap();
        assert!(read_mono_16k(&dir.join("bad.wav"), ChannelPick::Left).is_err());
        assert!(matches!(
            read_mono_16k(&dir.join("missing.wav"), ChannelPick::Left),
            Err(Error::Io(_))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}
