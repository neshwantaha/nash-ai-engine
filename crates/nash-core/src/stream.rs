//! Streaming event model.
//!
//! Adapters normalise every provider's streaming wire format into this small
//! set of events. The consumer (CLI, agent, UI) therefore renders streaming
//! output without knowing anything about SSE, NDJSON or provider quirks.

use crate::error::EngineError;
use crate::response::{FinishReason, Usage};
use futures::Stream;
use std::pin::Pin;

/// A single incremental event from a streaming completion.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// Emitted exactly once, before any content.
    Started { provider: String, model: String },
    /// Incremental assistant text.
    ContentDelta(String),
    /// Incremental reasoning/thinking text (separate channel from content).
    ReasoningDelta(String),
    /// A partial tool call. `index` identifies which call within the turn;
    /// `id`/`name` are typically sent once and `arguments_delta` accumulates
    /// a fragment of the JSON argument string.
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: String,
    },
    /// Token usage. Providers usually send this last.
    Usage(Usage),
    /// Terminal event carrying the finish reason.
    Done { finish_reason: FinishReason },
}

/// A boxed, `Send` stream of events.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, EngineError>> + Send>>;

/// Accumulates [`StreamEvent`]s into a complete response. Useful both for the
/// default `chat()` implementation and for the agent loop.
#[derive(Debug, Default)]
pub struct StreamCollector {
    pub content: String,
    pub reasoning: String,
    pub usage: Option<Usage>,
    pub finish_reason: FinishReason,
    tool_calls: Vec<PartialToolCall>,
}

#[derive(Debug, Default, Clone)]
struct PartialToolCall {
    id: Option<String>,
    name: Option<String>,
    args: String,
}

impl StreamCollector {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one event into the accumulated state.
    pub fn push(&mut self, event: &StreamEvent) {
        match event {
            StreamEvent::ContentDelta(t) => self.content.push_str(t),
            StreamEvent::ReasoningDelta(t) => self.reasoning.push_str(t),
            StreamEvent::ToolCallDelta {
                index,
                id,
                name,
                arguments_delta,
            } => {
                while self.tool_calls.len() <= *index {
                    self.tool_calls.push(PartialToolCall::default());
                }
                let slot = &mut self.tool_calls[*index];
                if let Some(id) = id {
                    if !id.is_empty() {
                        slot.id = Some(id.clone());
                    }
                }
                if let Some(name) = name {
                    if !name.is_empty() {
                        slot.name = Some(name.clone());
                    }
                }
                slot.args.push_str(arguments_delta);
            }
            StreamEvent::Usage(u) => self.usage = Some(u.clone()),
            StreamEvent::Done { finish_reason } => self.finish_reason = finish_reason.clone(),
            StreamEvent::Started { .. } => {}
        }
    }

    /// Produce the final tool calls, parsing accumulated JSON arguments.
    ///
    /// Invalid/blank argument strings become `null` rather than an error, so a
    /// malformed tool-argument payload cannot abort the whole turn silently.
    pub fn tool_calls(&self) -> Vec<crate::message::ToolCall> {
        self.tool_calls
            .iter()
            .filter_map(|tc| {
                let name = tc.name.clone()?;
                let id = tc.id.clone().unwrap_or_else(|| format!("call_{}", uuid::Uuid::new_v4()));
                let args = if tc.args.trim().is_empty() {
                    serde_json::Value::Object(serde_json::Map::new())
                } else {
                    serde_json::from_str(&tc.args).unwrap_or(serde_json::Value::Null)
                };
                Some(crate::message::ToolCall::new(id, name, args))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collects_content_and_finish() {
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::Started {
            provider: "p".into(),
            model: "m".into(),
        });
        c.push(&StreamEvent::ContentDelta("Hel".into()));
        c.push(&StreamEvent::ContentDelta("lo".into()));
        c.push(&StreamEvent::Done {
            finish_reason: FinishReason::Stop,
        });
        assert_eq!(c.content, "Hello");
        assert_eq!(c.finish_reason, FinishReason::Stop);
    }

    #[test]
    fn assembles_streamed_tool_call_arguments() {
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("call_a".into()),
            name: Some("get_weather".into()),
            arguments_delta: "{\"city\":".into(),
        });
        c.push(&StreamEvent::ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments_delta: "\"Paris\"}".into(),
        });
        let calls = c.tool_calls();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments["city"], "Paris");
    }

    #[test]
    fn malformed_tool_arguments_do_not_panic() {
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::ToolCallDelta {
            index: 0,
            id: Some("x".into()),
            name: Some("t".into()),
            arguments_delta: "{not json".into(),
        });
        let calls = c.tool_calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].arguments.is_null());
    }

    #[test]
    fn separates_reasoning_from_content() {
        let mut c = StreamCollector::new();
        c.push(&StreamEvent::ReasoningDelta("hmm".into()));
        c.push(&StreamEvent::ContentDelta("answer".into()));
        assert_eq!(c.reasoning, "hmm");
        assert_eq!(c.content, "answer");
    }
}
