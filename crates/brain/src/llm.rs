// SPDX-License-Identifier: GPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Andrea Mattera

//! The brain thinks through Home Assistant: its AI task (`ai_task.generate_data`), with the AI
//! integration and the key the user set up there. The device holds no key and calls no model.
//! Used by the memory jobs (end of a
//! conversation, the nightly tidy-up) and the morning summary; the conversation itself is Home
//! Assistant's agent, with the brain's tools over MCP. The earlier direct providers (Gemini,
//! Anthropic, OpenAI with a key on the device) are on the branch `standby/brain-direct-llm`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::config::AiCfg;
use crate::ha::Ha;

#[derive(Debug, Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub description: &'static str,
    /// JSON schema of the arguments (an object).
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum LlmError {
    /// No Home Assistant, or no token from the body yet: the brain cannot think.
    NoAi,
    /// No answer in time, or the connection failed: try again later.
    Unreachable(String),
    /// Home Assistant refused (no AI task entity, a bad request): not retried.
    Refused(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::NoAi => write!(f, "no AI: Home Assistant is not reachable yet"),
            LlmError::Unreachable(e) => write!(f, "unreachable: {e}"),
            LlmError::Refused(e) => write!(f, "refused: {e}"),
        }
    }
}

/// Home Assistant's AI task.
pub struct Client {
    cfg: AiCfg,
    ha: Option<Arc<Ha>>,
}

impl Client {
    pub fn new(cfg: AiCfg, ha: Option<Arc<Ha>>) -> Client {
        Client { cfg, ha }
    }

    /// The AI task entity configured ("" = the one chosen in Home Assistant, else the first).
    pub fn entity(&self) -> &str {
        &self.cfg.task_entity
    }

    /// Home Assistant is there with a token: worth asking.
    pub fn available(&self) -> bool {
        self.ha.as_ref().is_some_and(|h| h.has_token())
    }

    /// One answer, no tools: the instructions and the request as one text.
    pub fn complete(&self, system: &str, ask: &str) -> Result<String, LlmError> {
        let ha = self.ha.as_ref().ok_or(LlmError::NoAi)?;
        if !ha.has_token() {
            return Err(LlmError::NoAi);
        }
        let timeout = Duration::from_secs_f32(self.cfg.timeout_seconds);
        ha.generate(&format!("{system}\n\n{ask}"), &self.cfg.task_entity, timeout)
            .map_err(|e| {
                if e.contains("did not answer") || e.contains("connect") || e.contains("closed") {
                    LlmError::Unreachable(e)
                } else {
                    LlmError::Refused(e)
                }
            })
    }
}
