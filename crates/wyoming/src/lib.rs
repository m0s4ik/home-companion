// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The Wyoming protocol as Home Assistant speaks it to a voice satellite.
//!
//! - [`event`]: the framing (one JSON header line, an optional JSON data block, an optional
//!   binary payload), byte-compatible with `wyoming/event.py` 1.11.0;
//! - [`message`]: the typed events a satellite exchanges with Home Assistant;
//! - [`info`]: the `info` answer to `describe`;
//! - [`server`]: the satellite side (Home Assistant is the TCP client and connects to us);
//! - [`client`]: a small synchronous client, used by [`fake_ha`] and the tests;
//! - [`fake_ha`]: a scripted Home Assistant that drives a satellite through scenarios.
//!
//! No async runtime: plain `std::net` sockets and `std::thread`s. See README.md for the protocol
//! version, the sources this follows and the table of what is declared and handled.

pub mod client;
pub mod event;
pub mod fake_ha;
pub mod info;
pub mod message;
pub mod net;
pub mod server;
#[doc(hidden)]
pub mod stub;

/// The `version` written in every header line. Home Assistant core pins `wyoming==1.11.0`; the
/// wire format has not changed since 1.0, later versions only added fields.
pub const PROTOCOL_VERSION: &str = "1.11.0";

/// Largest data block or payload accepted by default (16 MiB).
pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Largest header line accepted (1 MiB).
pub const MAX_HEADER: usize = 1024 * 1024;

pub use client::Client;
pub use event::{Event, read_event, write_event};
pub use info::{Attribution, Info, SatelliteInfo, WakeModelInfo};
pub use message::{
    AudioFormat, Detection, Message, RunPipeline, Stage, Synthesize, TimerStarted, TimerUpdated,
    Transcript, Voice,
};
pub use net::Net;
pub use server::{Disconnect, Sender, Server, ServerConfig, ServerEvent};

/// Errors of this crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Header not JSON, missing `type`, lengths negative or oversized, truncated frame.
    #[error("bad frame: {0}")]
    Frame(String),
    #[error("not connected")]
    NotConnected,
}
