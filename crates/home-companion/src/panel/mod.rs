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
