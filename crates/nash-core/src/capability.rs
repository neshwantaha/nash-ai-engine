//! Capability model.
//!
//! The engine never assumes that two providers behave identically. Every
//! adapter declares what it supports using [`Capabilities`], and the runtime
//! refuses (with [`crate::EngineError::Unsupported`]) to attempt an operation a
//! provider has explicitly marked [`Support::Unsupported`].

use serde::{Deserialize, Serialize};

/// Tri-state support flag.
///
/// * `Supported`   - the adapter knows the feature works here.
/// * `Unsupported` - the adapter knows the feature is unavailable.
/// * `Unknown`     - not discovered. Callers may attempt the operation and
///   surface a clear error if it fails, but must not *promise* it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Support {
    Supported,
    Unsupported,
    #[default]
    Unknown,
}

impl Support {
    pub fn is_supported(self) -> bool {
        matches!(self, Support::Supported)
    }

    pub fn is_known_unsupported(self) -> bool {
        matches!(self, Support::Unsupported)
    }

    /// Combine two independent observations. `Unsupported` is sticky (if any
    /// source says no, the answer is no); otherwise `Unknown` dominates.
    pub fn and(self, other: Support) -> Support {
        match (self, other) {
            (Support::Unsupported, _) | (_, Support::Unsupported) => Support::Unsupported,
            (Support::Supported, Support::Supported) => Support::Supported,
            _ => Support::Unknown,
        }
    }
}

/// The complete capability declaration for a provider/model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Server-sent / chunked token streaming.
    pub streaming: Support,
    /// Tool / function calling.
    pub tools: Support,
    /// More than one tool call in a single assistant turn.
    pub parallel_tool_calls: Support,
    /// Strict JSON-schema constrained output.
    pub structured_output: Support,
    /// Loose JSON object mode (`response_format: json_object`).
    pub json_mode: Support,
    /// Image inputs.
    pub vision: Support,
    /// Audio inputs (transcription / speech understanding).
    pub audio_input: Support,
    /// Audio outputs (text-to-speech).
    pub audio_output: Support,
    /// Embedding generation.
    pub embeddings: Support,
    /// Explicit reasoning / thinking controls (o-series, Claude thinking, ...).
    pub reasoning: Support,
    /// Token usage reporting in the response.
    pub usage_reporting: Support,
    /// A `/models`-style discovery endpoint.
    pub model_listing: Support,
    /// Maximum input context window in tokens, when known.
    pub context_window: Option<u32>,
    /// Maximum output tokens, when known.
    pub max_output_tokens: Option<u32>,
    /// Human-readable caveats surfaced in the UI (e.g. "streaming may buffer").
    pub notes: Vec<String>,
}

impl Default for Capabilities {
    fn default() -> Self {
        // Conservative defaults: assume nothing works until an adapter says so.
        Self {
            streaming: Support::Unknown,
            tools: Support::Unknown,
            parallel_tool_calls: Support::Unknown,
            structured_output: Support::Unknown,
            json_mode: Support::Unknown,
            vision: Support::Unknown,
            audio_input: Support::Unknown,
            audio_output: Support::Unknown,
            embeddings: Support::Unknown,
            reasoning: Support::Unknown,
            usage_reporting: Support::Unknown,
            model_listing: Support::Unknown,
            context_window: None,
            max_output_tokens: None,
            notes: Vec::new(),
        }
    }
}

impl Capabilities {
    /// All-unknown builder.
    pub fn unknown() -> Self {
        Capabilities::default()
    }

    /// A reasonable baseline for the widely-implemented OpenAI Chat
    /// Completions shape. Individual endpoints must still be verified with a
    /// connection test before the UI promises these features.
    pub fn openai_chat_baseline() -> Self {
        Self {
            streaming: Support::Supported,
            tools: Support::Supported,
            parallel_tool_calls: Support::Supported,
            structured_output: Support::Supported,
            json_mode: Support::Supported,
            usage_reporting: Support::Supported,
            model_listing: Support::Supported,
            ..Capabilities::default()
        }
    }

    /// Merge manually-declared overrides on top of a detected/baseline set.
    /// `Supported`/`Unsupported` in `other` win; `Unknown` keeps `self`.
    pub fn overlay(&self, other: &Capabilities) -> Capabilities {
        fn pick(a: Support, b: Support) -> Support {
            match b {
                Support::Unknown => a,
                known => known,
            }
        }
        Capabilities {
            streaming: pick(self.streaming, other.streaming),
            tools: pick(self.tools, other.tools),
            parallel_tool_calls: pick(self.parallel_tool_calls, other.parallel_tool_calls),
            structured_output: pick(self.structured_output, other.structured_output),
            json_mode: pick(self.json_mode, other.json_mode),
            vision: pick(self.vision, other.vision),
            audio_input: pick(self.audio_input, other.audio_input),
            audio_output: pick(self.audio_output, other.audio_output),
            embeddings: pick(self.embeddings, other.embeddings),
            reasoning: pick(self.reasoning, other.reasoning),
            usage_reporting: pick(self.usage_reporting, other.usage_reporting),
            model_listing: pick(self.model_listing, other.model_listing),
            context_window: other.context_window.or(self.context_window),
            max_output_tokens: other.max_output_tokens.or(self.max_output_tokens),
            notes: {
                let mut n = self.notes.clone();
                n.extend(other.notes.iter().cloned());
                n
            },
        }
    }

    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_is_sticky_in_and() {
        assert_eq!(Support::Supported.and(Support::Unsupported), Support::Unsupported);
        assert_eq!(Support::Unknown.and(Support::Supported), Support::Unknown);
        assert_eq!(Support::Supported.and(Support::Supported), Support::Supported);
    }

    #[test]
    fn overlay_prefers_known_values() {
        let base = Capabilities::openai_chat_baseline();
        let override_caps = Capabilities {
            tools: Support::Unsupported,
            vision: Support::Supported,
            context_window: Some(128_000),
            ..Capabilities::default()
        };
        let merged = base.overlay(&override_caps);
        assert_eq!(merged.tools, Support::Unsupported);
        assert_eq!(merged.vision, Support::Supported);
        assert_eq!(merged.streaming, Support::Supported);
        assert_eq!(merged.context_window, Some(128_000));
    }
}
