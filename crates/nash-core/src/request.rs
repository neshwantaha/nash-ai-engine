//! Request model shared by all adapters.

use crate::message::{ChatMessage, ToolChoice, ToolDefinition};
use serde::{Deserialize, Serialize};

/// Structured-output request mode.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseFormat {
    #[default]
    Text,
    JsonObject,
    JsonSchema {
        name: String,
        schema: serde_json::Value,
        strict: bool,
    },
}

/// Tunable generation parameters. All optional; adapters only forward the
/// fields the target provider understands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct GenerationParams {
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    #[serde(alias = "max_output_tokens")]
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<String>,
    pub seed: Option<i64>,
    pub presence_penalty: Option<f32>,
    pub frequency_penalty: Option<f32>,
    /// Provider-specific reasoning control (e.g. `"low" | "medium" | "high"`,
    /// or a token budget string on some providers).
    pub reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "ResponseFormat::is_text")]
    pub response_format: ResponseFormat,
}

impl ResponseFormat {
    fn is_text(&self) -> bool {
        matches!(self, ResponseFormat::Text)
    }
}

/// A chat completion request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    #[serde(default)]
    pub tool_choice: ToolChoice,
    #[serde(default)]
    pub params: GenerationParams,
    /// Provider-specific passthrough fields merged into the request body.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ChatRequest {
    /// Start a request for `model` with no messages yet.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            messages: Vec::new(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            params: GenerationParams::default(),
            extra: serde_json::Map::new(),
        }
    }

    pub fn message(mut self, m: ChatMessage) -> Self {
        self.messages.push(m);
        self
    }

    pub fn messages(mut self, ms: impl IntoIterator<Item = ChatMessage>) -> Self {
        self.messages.extend(ms);
        self
    }

    pub fn tool(mut self, t: ToolDefinition) -> Self {
        self.tools.push(t);
        self
    }

    pub fn temperature(mut self, t: f32) -> Self {
        self.params.temperature = Some(t);
        self
    }

    pub fn max_tokens(mut self, n: u32) -> Self {
        self.params.max_tokens = Some(n);
        self
    }

    pub fn extra(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.extra.insert(key.into(), value);
        self
    }

    /// Whether this request contains anything the model must treat as tools.
    pub fn wants_tools(&self) -> bool {
        !self.tools.is_empty() && self.tool_choice != ToolChoice::None
    }

    /// True if any message carries an image (multimodal request).
    pub fn is_multimodal(&self) -> bool {
        self.messages.iter().any(ChatMessage::has_images)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_assembles_request() {
        let req = ChatRequest::new("gpt-4o")
            .message(ChatMessage::system("be terse"))
            .message(ChatMessage::user("hi"))
            .temperature(0.2)
            .max_tokens(64)
            .tool(ToolDefinition::new("get_time", "current time", serde_json::json!({"type":"object"})));

        assert_eq!(req.model, "gpt-4o");
        assert_eq!(req.messages.len(), 2);
        assert_eq!(req.params.temperature, Some(0.2));
        assert!(req.wants_tools());
        assert!(!req.is_multimodal());
    }

    #[test]
    fn tool_choice_none_disables_tools() {
        let mut req = ChatRequest::new("m").tool(ToolDefinition::new("t", "d", serde_json::json!({})));
        req.tool_choice = ToolChoice::None;
        assert!(!req.wants_tools());
    }
}
