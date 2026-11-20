// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The body's side of the browser panel (`home-companion-panel`, crates/panel): who is let in
//! ([`sessions`]), what may be asked ([`protocol`]), and the Unix socket the panel's process
//! asks on ([`socket`]). The panel's process serves the pages and holds no state or secret: every
//! request comes here, and the screen's thread answers it.

pub mod protocol;
pub mod sessions;
pub mod sha256;
pub mod socket;
pub mod widget_ops;

use serde::{Deserialize, Serialize};

/// `[panel]` in home-companion.toml. Whether the panel is on is the user's choice on the screen
/// (settings.toml `[panel] on`); this says where it is.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct PanelCfg {
    /// The port systemd listens on for the panel (home-companion-panel.socket), shown on the
    /// screen with the address. Change both together.
    pub port: u16,
    /// The body's socket the panel's process asks on.
    pub socket: String,
}

impl Default for PanelCfg {
    fn default() -> Self {
        PanelCfg {
            port: 8125,
            socket: socket::DEFAULT_PATH.into(),
        }
    }
}
