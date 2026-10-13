//! The provider abstraction.
//!
//! Every adapter implements [`Provider`]. The trait intentionally exposes a
//! small, uniform surface; provider-specific behaviour lives in the adapter's
//! request/response translation, not in the engine.

use crate::capability::Capabilities;
use crate::config::ProtocolKind;
use crate::error::Result;
use crate::request::ChatRequest;
use crate::response::ChatResponse;
use crate::stream::{EventStream, StreamCollector};
use async_trait::async_trait;
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use std::fmt;
use tokio_util::sync::CancellationToken;

/// A model advertised by a provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<i64>,
}

impl ModelInfo {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            display_name: None,
            context_window: None,
            max_output_tokens: None,
            owned_by: None,
            created: None,
        }
    }
}

/// A provider's coarse category (used by the UI and router).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderCategory {
    /// A first-party hosted provider (OpenAI, Anthropic, ...).
    Hosted,
    /// A multi-model aggregator (OpenRouter, ...).
    Aggregator,
    /// A local runtime (Ollama, LM Studio).
    Local,
    /// A self-hosted compatible server (vLLM, ...).
    SelfHosted,
    /// A user-defined endpoint.
    Custom,
}

/// Static description of a provider instance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
    pub protocol: ProtocolKind,
    pub category: ProviderCategory,
    pub base_url: String,
    pub local: bool,
    pub requires_api_key: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs_url: Option<String>,
}

/// Result of one connection check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckResult {
    pub name: String,
    pub status: CheckStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

impl CheckResult {
    pub fn pass(name: impl Into<String>, detail: impl Into<Option<String>>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Pass,
            detail: detail.into(),
            latency_ms: None,
        }
    }
    pub fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Fail,
            detail: Some(detail.into()),
            latency_ms: None,
        }
    }
    pub fn warn(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Warn,
            detail: Some(detail.into()),
            latency_ms: None,
        }
    }
    pub fn skip(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: CheckStatus::Skip,
            detail: Some(detail.into()),
            latency_ms: None,
        }
    }
    pub fn with_latency(mut self, ms: u64) -> Self {
        self.latency_ms = Some(ms);
        self
    }
}

/// Status of a single check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

/// A multi-step connection test report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ValidationReport {
    pub provider: String,
    pub checks: Vec<CheckResult>,
}

impl ValidationReport {
    pub fn new(provider: impl Into<String>) -> Self {
        Self {
            provider: provider.into(),
            checks: Vec::new(),
        }
    }

    pub fn push(&mut self, c: CheckResult) -> &mut Self {
        self.checks.push(c);
        self
    }

    pub fn passed(&self) -> bool {
        !self.checks.iter().any(|c| c.status == CheckStatus::Fail)
    }

    pub fn failed_checks(&self) -> impl Iterator<Item = &CheckResult> {
        self.checks.iter().filter(|c| c.status == CheckStatus::Fail)
    }
}

/// The uniform provider interface.
#[async_trait]
pub trait Provider: Send + Sync + fmt::Debug {
    /// Stable id (matches the config id).
    fn id(&self) -> &str;

    /// Static metadata.
    fn info(&self) -> ProviderInfo;

    /// Declared capabilities.
    fn capabilities(&self) -> Capabilities;

    /// Discover available models. May be unsupported (returns an error with
    /// kind `unsupported`).
    async fn list_models(&self, cancel: &CancellationToken) -> Result<Vec<ModelInfo>>;

    /// Stream a chat completion. The returned stream yields normalised events.
    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream>;

    /// A complete chat completion. Default implementation aggregates
    /// [`Provider::stream`], so adapters only implement one path.
    async fn chat(&self, req: ChatRequest, cancel: CancellationToken) -> Result<ChatResponse> {
        let info = self.info();
        let model = req.model.clone();
        let mut s = self.stream(req, cancel).await?;
        let mut collector = StreamCollector::new();
        while let Some(ev) = s.next().await {
            let ev = ev?;
            collector.push(&ev);
        }
        let mut resp = ChatResponse::new(info.id, model);
        resp.tool_calls = collector.tool_calls();
        resp.usage = collector.usage.clone().map(|u| u.normalised());
        resp.finish_reason = collector.finish_reason.clone();
        resp.content = std::mem::take(&mut collector.content);
        resp.reasoning = std::mem::take(&mut collector.reasoning);
        Ok(resp)
    }

    /// Run a connection/diagnostics test. Adapters override to add provider
    /// specific probes; the default reports nothing.
    async fn validate(&self, _cancel: &CancellationToken) -> Result<ValidationReport> {
        Ok(ValidationReport::new(self.id()))
    }

    /// Whether this provider is a local server.
    fn is_local(&self) -> bool {
        self.info().local
    }

    /// The model configured for this provider, if any. Used by the default
    /// connection-test helper.
    fn default_model(&self) -> Option<String> {
        None
    }
}
