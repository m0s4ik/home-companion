// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What an ALSA pcm accepts (rates, channels, formats, period and buffer limits), tested with
//! hw_params only: nothing is changed and no sound is made. Used by `home-companion audio-probe` and
//! `audio-bench probe`.

use crate::Error;

/// Pretty-printed JSON describing the pcm `device` ("hw:0,0") in one direction.
pub fn alsa_formats(device: &str, playback: bool) -> Result<String, Error> {
    #[cfg(all(target_os = "linux", feature = "alsa"))]
    {
        let v = crate::backend::alsa::probe(device, playback)
            .map_err(|e| Error::Device(e.to_string()))?;
        Ok(serde_json::to_string_pretty(&v).unwrap_or_default())
    }
    #[cfg(not(all(target_os = "linux", feature = "alsa")))]
    {
        let _ = (device, playback);
        Err(Error::Feature("alsa"))
    }
}
