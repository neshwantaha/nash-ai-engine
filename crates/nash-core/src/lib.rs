//! # Nash AI Engine — core
//!
//! A provider-agnostic AI engine. The [`Provider`] trait abstracts over
//! OpenAI-compatible APIs, Anthropic Messages, Google Gemini, Ollama,
//! custom REST endpoints and more. Adapters normalise everything into the
//! shared types in [`message`], [`request`], [`response`] and [`stream`].
//!
//! Design rules enforced by this crate:
//! * The core never depends on a UI or a database.
//! * Capabilities are declared, not assumed ([`capability`]).
//! * Every network destination passes through the [`security`] policy.
//! * Errors are normalised ([`error::EngineError`]) so callers can retry,
//!   cancel and diagnose uniformly.

pub mod adapters;
pub mod capability;
pub mod config;
pub mod engine;
pub mod error;
pub mod http;
pub mod message;
pub mod metrics;
pub mod provider;
pub mod registry;
pub mod request;
pub mod response;
pub mod routing;
pub mod secrets;
pub mod security;
pub mod stream;
pub mod template;
pub mod time;

pub use capability::{Capabilities, Support};
pub use config::{AppConfig, AuthConfig, ProtocolKind, ProviderConfig};
pub use engine::{Engine, RoutedStream};
pub use error::{EngineError, Result};
pub use http::{HttpClient, HttpClientConfig};
pub use message::{ChatMessage, ContentPart, Role, ToolCall, ToolChoice, ToolDefinition};
pub use metrics::{Metrics, ProviderMetrics};
pub use provider::{ModelInfo, Provider, ProviderInfo, ValidationReport};
pub use registry::{build_registry, ProviderRegistry};
pub use request::{ChatRequest, GenerationParams, ResponseFormat};
pub use response::{ChatResponse, FinishReason, Usage};
pub use routing::{rank, ProviderView, RouteRequest};
pub use secrets::{ChainedResolver, EnvSecretResolver, SecretResolver};
pub use stream::{EventStream, StreamCollector, StreamEvent};
pub use time::{now, now_rfc3339, parse_rfc3339};
