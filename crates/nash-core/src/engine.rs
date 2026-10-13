//! The engine facade.
//!
//! [`Engine`] owns the composed pieces — configuration, the HTTP client, the
//! provider registry, secrets and metrics — and exposes the handful of
//! operations the CLI, agent and UI actually need: route, chat, stream,
//! validate and list models. Routing and failover live here rather than in the
//! adapters, so every front end gets identical behaviour.

use crate::config::AppConfig;
use crate::error::{EngineError, Result};
use crate::http::{HttpClient, HttpClientConfig};
use crate::metrics::Metrics;
use crate::provider::{ModelInfo, Provider, ValidationReport};
use crate::registry::{build_registry, ProviderRegistry};
use crate::request::ChatRequest;
use crate::response::ChatResponse;
use crate::routing::{rank, ProviderView, RouteRequest};
use crate::secrets::SecretResolver;
use crate::stream::EventStream;
use futures::StreamExt;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// A streaming completion together with the provider that is serving it.
pub struct RoutedStream {
    pub provider: String,
    pub model: String,
    pub stream: EventStream,
}

/// The composed engine. Cheap to share behind an `Arc`.
pub struct Engine {
    config: AppConfig,
    registry: ProviderRegistry,
    http: HttpClient,
    metrics: Metrics,
    secrets: Box<dyn SecretResolver>,
    build_errors: Vec<(String, String)>,
}

impl Engine {
    /// Build an engine from configuration and a secret resolver.
    ///
    /// Individual providers that fail to construct are skipped and reported via
    /// [`Engine::build_errors`]; only a global failure (bad config, HTTP client)
    /// is fatal.
    pub fn new(config: AppConfig, secrets: Box<dyn SecretResolver>) -> Result<Self> {
        config.validate()?;
        let http = HttpClient::new(http_config(&config))?;
        let (registry, build_errors) = build_registry(&config, &http, secrets.as_ref())?;
        Ok(Self {
            config,
            registry,
            http,
            metrics: Metrics::new(),
            secrets,
            build_errors,
        })
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn registry(&self) -> &ProviderRegistry {
        &self.registry
    }

    pub fn http(&self) -> &HttpClient {
        &self.http
    }

    pub fn metrics(&self) -> &Metrics {
        &self.metrics
    }

    /// Providers that failed to build at startup, as `(id, message)`.
    pub fn build_errors(&self) -> &[(String, String)] {
        &self.build_errors
    }

    pub fn provider(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.registry.get(id)
    }

    pub fn provider_ids(&self) -> Vec<String> {
        self.registry.ids()
    }

    /// The default provider id, if configured.
    pub fn default_provider(&self) -> Option<String> {
        self.config
            .default_provider
            .clone()
            .filter(|id| self.registry.contains(id))
    }

    /// Preference order: the default provider first, then configuration order.
    fn preferred_order(&self) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(d) = self.default_provider() {
            out.push(d);
        }
        for id in self.registry.ids() {
            if !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// The secret resolver backing this engine (for rebuilding providers).
    pub fn secrets(&self) -> &dyn SecretResolver {
        self.secrets.as_ref()
    }

    /// Snapshot the registry as routing views.
    pub fn views(&self) -> Vec<ProviderView> {
        self.registry
            .all()
            .iter()
            .map(|p| {
                let models = self
                    .config
                    .provider(p.id())
                    .map(|c| c.models.clone())
                    .unwrap_or_default();
                ProviderView::from_provider(p.as_ref(), models)
            })
            .collect()
    }

    /// Rank providers for a request. Never returns empty while at least one
    /// provider exists: if the model filter removes everything (a model
    /// advertised nowhere), it falls back to preference order so the caller
    /// gets a clear per-provider error rather than "no route".
    pub fn route(&self, req: &RouteRequest) -> Vec<String> {
        if self.registry.is_empty() {
            return Vec::new();
        }
        let ranked = rank(
            &self.config.routing,
            &self.views(),
            req,
            &self.preferred_order(),
        );
        if ranked.is_empty() {
            self.preferred_order()
        } else {
            ranked
        }
    }

    /// Complete a chat request, routing and (optionally) failing over.
    pub async fn chat(&self, req: ChatRequest, cancel: CancellationToken) -> Result<ChatResponse> {
        let candidates = self.route(&RouteRequest {
            model: model_filter(&req),
            requires_tools: !req.tools.is_empty(),
            requires_vision: req.is_multimodal(),
            ..Default::default()
        });

        let mut last_err: Option<EngineError> = None;
        for id in &candidates {
            let provider = match self.provider(id) {
                Some(p) => p,
                None => continue,
            };
            let model = match resolve_model(&provider, &req) {
                Ok(m) => m,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };
            let mut attempt = req.clone();
            attempt.model = model;
            let started = std::time::Instant::now();
            match provider.chat(attempt, cancel.clone()).await {
                Ok(resp) => {
                    let usage = resp.usage.clone().unwrap_or_default();
                    self.metrics.record_response(id, started, &usage);
                    return Ok(resp);
                }
                Err(EngineError::Cancelled) => {
                    self.metrics.record_cancellation(id);
                    return Err(EngineError::Cancelled);
                }
                Err(e) => {
                    self.metrics.record_error(id);
                    last_err = Some(e);
                    if !self.config.routing.allow_failover {
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| EngineError::Config("no provider available".into())))
    }

    /// Open a streaming completion, routing and (optionally) failing over on
    /// connection errors. Errors that occur after the stream is established are
    /// surfaced through the stream itself, not here.
    pub async fn chat_stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<RoutedStream> {
        let candidates = self.route(&RouteRequest {
            model: model_filter(&req),
            requires_tools: !req.tools.is_empty(),
            requires_vision: req.is_multimodal(),
            ..Default::default()
        });
        self.select_stream(candidates, req, cancel).await
    }

    /// Run a provider's connection/diagnostics test.
    pub async fn validate(&self, id: &str, cancel: &CancellationToken) -> Result<ValidationReport> {
        let provider = self
            .provider(id)
            .ok_or_else(|| EngineError::Config(format!("unknown provider '{id}'")))?;
        provider.validate(cancel).await
    }

    /// Validate every registered provider concurrently.
    pub async fn validate_all(&self, cancel: &CancellationToken) -> Vec<ValidationReport> {
        let mut reports = Vec::new();
        for provider in self.registry.all() {
            match provider.validate(cancel).await {
                Ok(report) => reports.push(report),
                Err(e) => {
                    let mut r = ValidationReport::new(provider.id());
                    r.push(crate::provider::CheckResult::fail("connection", e.to_string()));
                    reports.push(r);
                }
            }
        }
        reports
    }

    /// List models for a provider.
    pub async fn list_models(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<ModelInfo>> {
        let provider = self
            .provider(id)
            .ok_or_else(|| EngineError::Config(format!("unknown provider '{id}'")))?;
        provider.list_models(cancel).await
    }

    /// Failover driver for streaming requests.
    async fn select_stream(
        &self,
        candidates: Vec<String>,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> Result<RoutedStream> {
        let mut last_err: Option<EngineError> = None;
        for id in &candidates {
            let provider = match self.provider(id) {
                Some(p) => p,
                None => continue,
            };
            let model = match resolve_model(&provider, &req) {
                Ok(m) => m,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };
            let mut attempt = req.clone();
            attempt.model = model.clone();
            match provider.stream(attempt, cancel.clone()).await {
                Ok(stream) => {
                    let provider_id = provider.id().to_string();
                    let metrics = self.metrics.clone();
                    let pid = provider_id.clone();
                    let stream = stream.map(move |ev| {
                        if let Ok(e) = &ev {
                            metrics.record_stream_event(&pid, e);
                        }
                        ev
                    });
                    return Ok(RoutedStream {
                        provider: provider_id,
                        model,
                        stream: Box::pin(stream),
                    });
                }
                Err(EngineError::Cancelled) => {
                    self.metrics.record_cancellation(id);
                    return Err(EngineError::Cancelled);
                }
                Err(e) => {
                    self.metrics.record_error(id);
                    last_err = Some(e);
                    if !self.config.routing.allow_failover {
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| EngineError::Config("no provider available".into())))
    }
}

fn resolve_model(provider: &Arc<dyn Provider>, req: &ChatRequest) -> Result<String> {
    if !req.model.trim().is_empty() {
        return Ok(req.model.clone());
    }
    provider.default_model().ok_or_else(|| {
        EngineError::Config(format!(
            "provider '{}' has no default model; set one in configuration or name a model",
            provider.id()
        ))
    })
}

fn model_filter(req: &ChatRequest) -> Option<String> {
    let m = req.model.trim();
    if m.is_empty() {
        None
    } else {
        Some(m.to_string())
    }
}

/// Build the HTTP client configuration from the app config's security policy.
fn http_config(config: &AppConfig) -> HttpClientConfig {
    HttpClientConfig {
        security: Arc::new(config.security.clone().into()),
        ..Default::default()
    }
}
