// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! Reads a 16 kHz S16 WAV file (no resampling here). Mono files are read as they are; a
//! multi-channel file (the device microphone captures 2 channels) needs an explicit channel.

use crate::{Error, SAMPLE_RATE};
use std::io;
use std::path::Path;

fn invalid(path: &Path, why: impl std::fmt::Display) -> Error {
    Error::Io(io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{}: {why}", path.display()),
    ))
}

/// The samples of one channel of a 16 kHz, 16-bit PCM WAV. `channel` may be `None` (or `Some(0)`)
/// for a mono file and must be given for a multi-channel one.
pub fn read_16k(path: &Path, channel: Option<usize>) -> Result<Vec<i16>, Error> {
    let r = hound::WavReader::open(path).map_err(|e| match e {
        hound::Error::IoError(e) => {
            Error::Io(io::Error::new(e.kind(), format!("{}: {e}", path.display())))
        }
        e => invalid(path, e),
    })?;
    let s = r.spec();
    if s.sample_rate != SAMPLE_RATE
        || s.bits_per_sample != 16
        || s.sample_format != hound::SampleFormat::Int
    {
        return Err(invalid(
            path,
            format!(
                "need {SAMPLE_RATE} Hz 16-bit PCM, got {} Hz {}-bit {:?}",
                s.sample_rate, s.bits_per_sample, s.sample_format
            ),
        ));
    }
    let ch = s.channels as usize;
    let pick = match (ch, channel) {
        (0, _) => return Err(invalid(path, "no channels")),
        (1, None | Some(0)) => 0,
        (_, Some(c)) if c < ch => c,
        (_, Some(c)) => {
            return Err(invalid(
                path,
                format!("has {ch} channel(s), no channel {c}"),
            ));
        }
        (_, None) => return Err(invalid(path, format!("has {ch} channels; choose one"))),
    };
    let all: Vec<i16> = r
        .into_samples::<i16>()
        .collect::<Result<_, _>>()
        .map_err(|e| invalid(path, e))?;
    Ok(all.into_iter().skip(pick).step_by(ch).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, rate: u32, channels: u16, frames: &[[i16; 2]]) {
        let spec = hound::WavSpec {
            channels,
            sample_rate: rate,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(path, spec).unwrap();
        for f in frames {
            for &s in &f[..channels as usize] {
                w.write_sample(s).unwrap();
            }
        }
        w.finalize().unwrap();
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("wakeword-wav-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join(name)
    }

    #[test]
    fn rejects_22050_hz() {
        let p = tmp("22k.wav");
        write(&p, 22_050, 1, &[[1, 0], [2, 0]]);
        let e = read_16k(&p, None).unwrap_err();
        assert!(e.to_string().contains("22050"), "{e}");
    }

    #[test]
    fn selects_a_channel_of_a_stereo_file() {
        let p = tmp("stereo.wav");
        write(&p, 16_000, 2, &[[1, -1], [2, -2], [3, -3]]);
        assert_eq!(read_16k(&p, Some(0)).unwrap(), vec![1, 2, 3]);
        assert_eq!(read_16k(&p, Some(1)).unwrap(), vec![-1, -2, -3]);
        assert!(read_16k(&p, None).is_err(), "a stereo file needs a channel");
        assert!(read_16k(&p, Some(2)).is_err());
    }

    #[test]
    fn reads_mono() {
        let p = tmp("mono.wav");
        write(&p, 16_000, 1, &[[5, 0], [6, 0]]);
        assert_eq!(read_16k(&p, None).unwrap(), vec![5, 6]);
        assert_eq!(read_16k(&p, Some(0)).unwrap(), vec![5, 6]);
    }

    #[test]
    fn missing_file_is_an_io_error() {
        assert!(matches!(
            read_16k(Path::new("/nonexistent/x.wav"), None),
            Err(Error::Io(_))
        ));
    }
}
