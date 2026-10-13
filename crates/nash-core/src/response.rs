//! Response model and token accounting.

use crate::message::ToolCall;
use serde::{Deserialize, Serialize};

/// Token usage, when the provider reports it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
    /// Reasoning/thinking tokens, when reported separately.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    /// Tokens served from a provider-side prompt cache.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
}

impl Usage {
    /// Create usage from the two token counts, filling `total_tokens`.
    pub fn new(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens + completion_tokens,
            ..Default::default()
        }
        .normalised()
    }

    /// Fill `total_tokens` if the provider omitted it.
    pub fn normalised(mut self) -> Self {
        if self.total_tokens == 0 {
            self.total_tokens = self.prompt_tokens + self.completion_tokens;
        }
        self
    }
}

/// Why generation stopped.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    #[default]
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

impl FinishReason {
    /// Map common provider spellings onto the canonical variants.
    pub fn parse(raw: &str) -> Self {
        match raw {
            "stop" | "end_turn" | "STOP" => FinishReason::Stop,
            "length" | "max_tokens" | "max_output_tokens" => FinishReason::Length,
            "tool_calls" | "function_call" | "tool_use" => FinishReason::ToolCalls,
            "content_filter" | "safety" | "SAFETY" => FinishReason::ContentFilter,
            other => FinishReason::Other(other.to_string()),
        }
    }
}

/// A complete (non-streamed) chat response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub provider: String,
    pub model: String,
    #[serde(default)]
    pub content: String,
    /// Reasoning/thinking text, when the provider exposes it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reasoning: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub finish_reason: FinishReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

impl ChatResponse {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            ..Default::default()
        }
    }

    /// Whether the model asked to call one or more tools.
    pub fn wants_tools(&self) -> bool {
        !self.tool_calls.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn total_tokens_is_filled_in() {
        let u = Usage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 0,
            ..Default::default()
        }
        .normalised();
        assert_eq!(u.total_tokens, 15);
    }

    #[test]
    fn finish_reason_aliases() {
        assert_eq!(FinishReason::parse("end_turn"), FinishReason::Stop);
        assert_eq!(FinishReason::parse("max_tokens"), FinishReason::Length);
        assert_eq!(FinishReason::parse("tool_use"), FinishReason::ToolCalls);
        assert_eq!(
            FinishReason::parse("weird"),
            FinishReason::Other("weird".into())
        );
    }
}
