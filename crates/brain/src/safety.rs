// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! What may reach the model and what the model may do.
//!
//! - **External text** (web search, Home Assistant's answers and states) is cleaned before the model
//!   sees it: Unicode TAG characters (U+E0000-U+E007F) and invisible format characters, used to hide
//!   instructions, are removed, control characters too, and the length is capped.
//! - **House commands** go through a gate (OpenDots' read-only/mutating split): domains that are
//!   never controlled by voice (default: alarms and locks) are refused; sensitive ones (default:
//!   covers: shutters, garage doors, gates) need a spoken "Confermi?" and an explicit yes in the
//!   next turn, executed by the brain itself, not by the model. The command is free text, so the
//!   domains are recognised by their words (Italian and English).

pub use ::safety::{Gate, SafetyCfg};

fn invisible(c: char) -> bool {
    matches!(c,
        '\u{E0000}'..='\u{E007F}'
        | '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}'
        | '\u{FE00}'..='\u{FE0F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}'
        | '\u{E0100}'..='\u{E01EF}')
        || (c.is_control() && c != '\n' && c != '\t')
}

/// External text for the model: invisible and control characters out, at most `max` characters.
pub fn sanitize(s: &str, max: usize) -> String {
    let clean: String = s.chars().filter(|c| !invisible(*c)).collect();
    if clean.chars().count() <= max {
        return clean;
    }
    let mut t: String = clean.chars().take(max).collect();
    t.push('…');
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_characters_go() {
        let tagged = format!("ciao{}fine", "\u{E0049}\u{E0047}\u{E004E}");
        assert_eq!(sanitize(&tagged, 100), "ciaofine");
        assert_eq!(sanitize("a\u{200B}b\u{202E}c\u{0007}", 100), "abc");
        assert_eq!(sanitize("abcdef", 3), "abc…");
    }
}
