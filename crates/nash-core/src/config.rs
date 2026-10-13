//! Versioned configuration schema.
//!
//! Secrets are **never** stored inline as raw values in an [`AppConfig`].
//! Instead a provider references a secret by name ([`ProviderConfig::secret_ref`])
//! which is resolved from the OS keyring (or the `NASH_SECRET_<NAME>` env var)
//! at engine build time. This makes it safe to export/import configuration.

use crate::error::{EngineError, Result};
use crate::http::PreparedRequest;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Current configuration schema version.
pub const CONFIG_SCHEMA_VERSION: u32 = 1;

/// The wire protocol an adapter speaks.
///
/// The serde names intentionally match [`ProtocolKind::as_str`] so a config
/// round-trips through either representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolKind {
    /// OpenAI `POST /chat/completions` (also used by Azure, OpenRouter,
    /// DeepSeek, Groq, Mistral, xAI, vLLM, LM Studio, Together, ...).
    #[serde(rename = "openai_chat")]
    OpenAiChat,
    /// OpenAI `POST /responses`.
    #[serde(rename = "openai_responses")]
    OpenAiResponses,
    /// Anthropic `POST /v1/messages`.
    #[serde(rename = "anthropic_messages")]
    AnthropicMessages,
    /// Google Gemini `generateContent` / `streamGenerateContent`.
    #[serde(rename = "google_gemini")]
    GoogleGemini,
    /// Ollama native `POST /api/chat`.
    #[serde(rename = "ollama")]
    Ollama,
    /// A user-defined mapping over an arbitrary JSON HTTP endpoint.
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    /// A generic REST endpoint that is *not* treated as a model endpoint.
    #[serde(rename = "custom_rest")]
    CustomRest,
}

impl ProtocolKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProtocolKind::OpenAiChat => "openai_chat",
            ProtocolKind::OpenAiResponses => "openai_responses",
            ProtocolKind::AnthropicMessages => "anthropic_messages",
            ProtocolKind::GoogleGemini => "google_gemini",
            ProtocolKind::Ollama => "ollama",
            ProtocolKind::OpenAiCompatible => "openai_compatible",
            ProtocolKind::CustomRest => "custom_rest",
        }
    }

    /// Whether this protocol performs model inference (chat).
    pub fn is_inference(self) -> bool {
        !matches!(self, ProtocolKind::CustomRest)
    }
}

/// How to authenticate a request.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthConfig {
    None,
    /// `Authorization: Bearer <secret>`
    #[default]
    Bearer,
    /// `<header>: <secret>`
    ApiKeyHeader { header: String },
    /// Append `<name>=<secret>` to the query string.
    QueryParam { name: String },
    /// `Authorization: Basic <base64(secret)>` (secret is `user:pass`).
    Basic,
    /// `<header>: <prefix><secret>`
    Custom { header: String, prefix: Option<String> },
}

/// Serializable retry configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter: bool,
    pub respect_retry_after: bool,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay_ms: 500,
            max_delay_ms: 30_000,
            jitter: true,
            respect_retry_after: true,
        }
    }
}

impl From<RetryConfig> for crate::http::RetryPolicy {
    fn from(r: RetryConfig) -> Self {
        crate::http::RetryPolicy {
            max_attempts: r.max_attempts.max(1),
            base_delay: std::time::Duration::from_millis(r.base_delay_ms),
            max_delay: std::time::Duration::from_millis(r.max_delay_ms),
            jitter: r.jitter,
            respect_retry_after: r.respect_retry_after,
        }
    }
}

/// JSON-request mapping for a custom endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct RequestTemplate {
    /// Field name that carries the model id. Empty = don't send.
    #[serde(default)]
    pub model_field: String,
    /// Field name that carries the messages array. Empty = don't send.
    #[serde(default)]
    pub messages_field: String,
    /// Field name that carries the streaming flag. Empty = don't send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_field: Option<String>,
    /// A full body template. When present it is rendered with placeholders
    /// (`{{model}}`, `{{messages}}`, `{{prompt}}`, `{{stream}}`,
    /// `{{temperature}}`, `{{max_tokens}}`, `{{tools}}`, `{{tool_choice}}`)
    /// and takes precedence over the field-name shortcuts above.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<serde_json::Value>,
    /// Static fields always merged into the request body.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// How to extract values from a custom response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ResponseMapping {
    /// Dotted JSON path to the assistant text, e.g. `choices.0.message.content`.
    pub text_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason_path: Option<String>,
}

/// How a custom endpoint streams.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamMapping {
    /// No streaming; the endpoint returns a single JSON body.
    #[default]
    None,
    /// Server-Sent Events carrying JSON objects; extract text at `text_path`.
    SseJson { text_path: String },
    /// Newline-delimited JSON; extract text at `text_path`.
    Ndjson { text_path: String },
    /// Server-Sent Events carrying raw text payloads (no JSON).
    SseText,
}

/// A fully user-defined inference endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointTemplate {
    /// Path appended to `base_url` for chat, e.g. `/chat/completions`.
    pub chat_path: String,
    /// Path for model discovery, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_path: Option<String>,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub request: RequestTemplate,
    #[serde(default)]
    pub response: ResponseMapping,
    #[serde(default)]
    pub stream: StreamMapping,
    /// Manually declared capabilities (overrides auto-detection).
    #[serde(default)]
    pub capabilities: crate::capability::Capabilities,
}

fn default_method() -> String {
    "POST".to_string()
}

/// One provider instance in the configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderConfig {
    /// Stable identifier used everywhere (unique within the config).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub protocol: ProtocolKind,
    /// Base URL, e.g. `https://api.openai.com/v1`, `http://localhost:11434`.
    pub base_url: String,
    #[serde(default)]
    pub auth: AuthConfig,
    /// Name of the secret in the OS keyring / `NASH_SECRET_<NAME>` env var.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_ref: Option<String>,
    /// Extra static headers.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Extra static query parameters.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, String>,
    /// Preferred/selected model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Known/allow-listed models (for the selector and routing).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub retry: RetryConfig,
    /// Max concurrent requests to this provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub concurrency: Option<usize>,
    /// Overrides applied on top of detected/baseline capabilities.
    #[serde(default)]
    pub capabilities: crate::capability::Capabilities,
    /// Custom endpoint definition (required for `openai_compatible`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<EndpointTemplate>,
    /// Mark this provider as a local server (affects privacy routing + UI).
    #[serde(default)]
    pub local: bool,
    /// Free-form tags used by the router (e.g. `["fast","cheap","private"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl ProviderConfig {
    /// Validate static consistency before the provider is built.
    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(EngineError::Config("provider id must not be empty".into()));
        }
        if self.base_url.trim().is_empty() {
            return Err(EngineError::Config(format!(
                "provider '{}' has an empty base_url",
                self.id
            )));
        }
        let url = url::Url::parse(&self.base_url).map_err(|e| {
            EngineError::Config(format!("provider '{}' has an invalid base_url: {e}", self.id))
        })?;
        if url.scheme() != "http" && url.scheme() != "https" {
            return Err(EngineError::Config(format!(
                "provider '{}' base_url must be http or https",
                self.id
            )));
        }
        if self.protocol == ProtocolKind::OpenAiCompatible {
            let ep = self.endpoint.as_ref().ok_or_else(|| {
                EngineError::Config(format!(
                    "provider '{}' uses openai_compatible but has no endpoint template",
                    self.id
                ))
            })?;
            if ep.chat_path.is_empty() {
                return Err(EngineError::Config(format!(
                    "provider '{}' endpoint.chat_path must not be empty",
                    self.id
                )));
            }
            if ep.response.text_path.is_empty()
                && matches!(ep.stream, StreamMapping::None)
            {
                return Err(EngineError::Config(format!(
                    "provider '{}' endpoint.response.text_path must be set for non-streaming responses",
                    self.id
                )));
            }
        }
        if self.auth != AuthConfig::None
            && self.secret_ref.is_none()
            && !self.local
        {
            return Err(EngineError::Config(format!(
                "provider '{}' requires a secret_ref (set auth to none for unauthenticated endpoints)",
                self.id
            )));
        }
        Ok(())
    }
}

/// Routing preferences.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingConfig {
    /// Ordered provider preferences; earlier wins when scores tie.
    #[serde(default)]
    pub preferred: Vec<String>,
    /// Allow automatic failover to another provider on failure.
    #[serde(default)]
    pub allow_failover: bool,
    /// Never route to a provider that is not marked `local` when privacy is
    /// required. (Privacy-required requests only use local providers.)
    #[serde(default)]
    pub privacy_only_local: bool,
    /// Soft per-request cost ceiling (informational; providers without pricing
    /// data are treated as free).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        Self {
            preferred: Vec::new(),
            allow_failover: true,
            privacy_only_local: true,
            max_cost_usd: None,
        }
    }
}

/// Security configuration.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SecurityConfig {
    #[serde(default)]
    pub allow_insecure_http: bool,
    #[serde(default)]
    pub allow_private_networks: bool,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl From<SecurityConfig> for crate::security::SecurityPolicy {
    fn from(s: SecurityConfig) -> Self {
        crate::security::SecurityPolicy {
            allow_insecure_http: s.allow_insecure_http,
            allow_private_networks: s.allow_private_networks,
            allowed_hosts: s.allowed_hosts,
            block_metadata_endpoints: true,
        }
    }
}

/// The whole application configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_provider: Option<String>,
    #[serde(default)]
    pub routing: RoutingConfig,
    #[serde(default)]
    pub security: SecurityConfig,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            providers: Vec::new(),
            default_provider: None,
            routing: RoutingConfig::default(),
            security: SecurityConfig::default(),
        }
    }
}

impl AppConfig {
    /// A batteries-included starting configuration.
    ///
    /// Seeds the common providers so a fresh install has something usable
    /// immediately: a local Ollama server (no secret needed) plus the hosted
    /// providers, each referencing a keyring secret by name. Nothing here
    /// embeds a credential.
    pub fn starter() -> Self {
        fn provider(
            id: &str,
            display_name: &str,
            protocol: ProtocolKind,
            base_url: &str,
            model: &str,
            secret_ref: Option<&str>,
            local: bool,
        ) -> ProviderConfig {
            ProviderConfig {
                id: id.to_string(),
                display_name: Some(display_name.to_string()),
                protocol,
                base_url: base_url.to_string(),
                auth: AuthConfig::default(),
                secret_ref: secret_ref.map(str::to_string),
                headers: BTreeMap::new(),
                query: BTreeMap::new(),
                model: Some(model.to_string()),
                models: Vec::new(),
                timeout_ms: None,
                retry: RetryConfig::default(),
                concurrency: None,
                capabilities: Default::default(),
                endpoint: None,
                local,
                tags: Vec::new(),
                enabled: true,
            }
        }

        // Free-model lists captured from each gateway's live `/models`
        // endpoint. The in-app "Models" button re-fetches these, so a stale
        // list only affects the starter config.
        //
        // OpenCode Zen free models that speak OpenAI chat-completions. The one
        // remaining free model, `muse-spark-1.3-contributor-free`, uses the
        // OpenAI Responses API instead, so it lives on its own provider below.
        // (`jev-1.13-free` uses a non-chat System One API and is not listed.)
        let opencode_free = vec![
            "big-pickle",
            "space-bunny-free",
            "cat-jam-free",
            "longcat-2.5-preview-free",
            "step-5-preview-free",
            "exo-free",
            "mimo-v2.6-flash-free",
            "mimo-v2.5-free",
            "ling-3.1-flash-free",
            "ling-3.0-flash-fin-free",
            "nemotron-3-ultra-free",
            "nemotron-3.5-lightning-free",
        ];
        let openrouter_free = vec![
            "apodex/apodex-1.1-mini:free",
            "cohere/north-mini-code:free",
            "dots-studio/dots-3-note-preview:free",
            "google/gemma-4-26b-a4b-it:free",
            "google/gemma-4-31b-it:free",
            "liquid/lfm-2.5-2.6b:free",
            "nvidia/nemotron-3.5-content-safety:free",
            "nvidia/nemotron-3.5-lightning:free",
            "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning:free",
            "nvidia/nemotron-3-super-120b-a12b:free",
            "nvidia/nemotron-3-ultra-550b-a55b:free",
            "openrouter/free",
            "poolside/laguna-s-2.1:free",
            "poolside/laguna-xs-2.1:free",
            "thinkingmachines/inkling:free",
            "thinkingmachines/inkling-small:free",
        ];

        // OpenCode Zen is an OpenAI-compatible gateway. GPT models live on
        // `/responses` and Claude/Gemini on `/messages`/Google; switch the
        // `protocol` (and model) to reach those. The seeded defaults are free
        // chat-completions models so they work without billing enabled.
        let mut opencode = provider(
            "opencode",
            "OpenCode Zen",
            ProtocolKind::OpenAiChat,
            "https://opencode.ai/inference/v1",
            "big-pickle",
            Some("opencode_api_key"),
            false,
        );
        opencode.models = opencode_free.into_iter().map(String::from).collect();

        // The one free OpenCode model served via the OpenAI Responses API.
        let mut opencode_muse = provider(
            "opencode-muse",
            "OpenCode Zen (Muse)",
            ProtocolKind::OpenAiResponses,
            "https://opencode.ai/inference/v1",
            "muse-spark-1.3-contributor-free",
            Some("opencode_api_key"),
            false,
        );
        opencode_muse.models = vec!["muse-spark-1.3-contributor-free".to_string()];

        let mut openrouter = provider(
            "openrouter",
            "OpenRouter",
            ProtocolKind::OpenAiChat,
            "https://openrouter.ai/api/v1",
            "google/gemma-4-31b-it:free",
            Some("openrouter_api_key"),
            false,
        );
        openrouter.models = openrouter_free.into_iter().map(String::from).collect();
        openrouter
            .headers
            .insert("X-Title".to_string(), "Nash AI Engine".to_string());

        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            providers: vec![
                provider(
                    "ollama",
                    "Ollama (local)",
                    ProtocolKind::Ollama,
                    "http://localhost:11434",
                    "llama3.2",
                    None,
                    true,
                ),
                provider(
                    "openai",
                    "OpenAI",
                    ProtocolKind::OpenAiChat,
                    "https://api.openai.com/v1",
                    "gpt-4o-mini",
                    Some("openai_api_key"),
                    false,
                ),
                provider(
                    "anthropic",
                    "Anthropic",
                    ProtocolKind::AnthropicMessages,
                    "https://api.anthropic.com/v1",
                    "claude-3-5-sonnet-latest",
                    Some("anthropic_api_key"),
                    false,
                ),
                provider(
                    "gemini",
                    "Google Gemini",
                    ProtocolKind::GoogleGemini,
                    "https://generativelanguage.googleapis.com/v1beta",
                    "gemini-1.5-flash",
                    Some("google_api_key"),
                    false,
                ),
                opencode,
                opencode_muse,
                openrouter,
            ],
            default_provider: Some("ollama".to_string()),
            routing: RoutingConfig::default(),
            security: SecurityConfig::default(),
        }
    }

    /// Validate the whole config (version + every provider).
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(EngineError::Config(format!(
                "unsupported config schema version {} (expected {})",
                self.schema_version, CONFIG_SCHEMA_VERSION
            )));
        }
        let mut seen = std::collections::HashSet::new();
        for p in &self.providers {
            if !seen.insert(p.id.clone()) {
                return Err(EngineError::Config(format!(
                    "duplicate provider id '{}'",
                    p.id
                )));
            }
            p.validate()?;
        }
        if let Some(d) = &self.default_provider {
            if !self.providers.iter().any(|p| &p.id == d) {
                return Err(EngineError::Config(format!(
                    "default_provider '{d}' does not match any configured provider"
                )));
            }
        }
        Ok(())
    }

    /// Parse from JSON with version checking.
    pub fn from_json(text: &str) -> Result<Self> {
        let cfg: AppConfig = serde_json::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn to_json_pretty(&self) -> Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    /// A config suitable for export: identical but explicitly documents that
    /// it contains no secrets (it never does — only `secret_ref` names).
    pub fn export_json(&self) -> Result<String> {
        self.to_json_pretty()
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }
}

/// Apply authentication to a prepared request. `secret` is the resolved
/// credential (already fetched from the secret store); it is never logged.
pub fn apply_auth(
    mut req: PreparedRequest,
    auth: &AuthConfig,
    secret: Option<&str>,
) -> Result<PreparedRequest> {
    let needs_secret = !matches!(auth, AuthConfig::None);
    if needs_secret && secret.map(str::is_empty).unwrap_or(true) {
        return Err(EngineError::Auth(format!(
            "provider requires a credential for auth type '{}' but none is configured",
            auth_kind(auth)
        )));
    }
    match auth {
        AuthConfig::None => {}
        AuthConfig::Bearer => {
            req = req.bearer(secret.unwrap());
        }
        AuthConfig::ApiKeyHeader { header } => {
            req = req.header(header.clone(), secret.unwrap());
        }
        AuthConfig::QueryParam { name } => {
            let mut url = req.url.clone();
            url.query_pairs_mut().append_pair(name, secret.unwrap());
            req.url = url;
        }
        AuthConfig::Basic => {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(secret.unwrap());
            req = req.header("authorization", format!("Basic {encoded}"));
        }
        AuthConfig::Custom { header, prefix } => {
            let value = match prefix {
                Some(p) => format!("{p}{}", secret.unwrap()),
                None => secret.unwrap().to_string(),
            };
            req = req.header(header.clone(), value);
        }
    }
    Ok(req)
}

fn auth_kind(auth: &AuthConfig) -> &'static str {
    match auth {
        AuthConfig::None => "none",
        AuthConfig::Bearer => "bearer",
        AuthConfig::ApiKeyHeader { .. } => "api_key_header",
        AuthConfig::QueryParam { .. } => "query_param",
        AuthConfig::Basic => "basic",
        AuthConfig::Custom { .. } => "custom",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            display_name: None,
            protocol: ProtocolKind::OpenAiChat,
            base_url: "https://api.example.com/v1".into(),
            auth: AuthConfig::Bearer,
            secret_ref: Some(format!("{id}-key")),
            headers: BTreeMap::new(),
            query: BTreeMap::new(),
            model: None,
            models: vec![],
            timeout_ms: None,
            retry: RetryConfig::default(),
            concurrency: None,
            capabilities: crate::capability::Capabilities::default(),
            endpoint: None,
            local: false,
            tags: vec![],
            enabled: true,
        }
    }

    #[test]
    fn default_config_is_valid() {
        let cfg = AppConfig::default();
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn starter_config_is_valid_and_local_ready() {
        let cfg = AppConfig::starter();
        assert!(cfg.validate().is_ok());
        let ollama = cfg.provider("ollama").expect("ollama seeded");
        assert!(ollama.local);
        assert!(ollama.secret_ref.is_none());
        assert_eq!(cfg.default_provider.as_deref(), Some("ollama"));
        assert!(cfg.providers.iter().any(|p| p.id == "openai"));

        let oc = cfg.provider("opencode").expect("opencode seeded");
        assert!(oc.models.iter().all(|m| m.ends_with("-free") || m == "big-pickle"));
        assert!(oc.models.contains(&oc.model.clone().unwrap()));

        let muse = cfg.provider("opencode-muse").expect("opencode-muse seeded");
        assert_eq!(muse.protocol, ProtocolKind::OpenAiResponses);
        assert!(muse.models.contains(&muse.model.clone().unwrap()));

        let or = cfg.provider("openrouter").expect("openrouter seeded");
        assert_eq!(or.base_url, "https://openrouter.ai/api/v1");
        assert!(or.models.iter().all(|m| m.ends_with(":free") || m == "openrouter/free"));
        assert!(or.models.contains(&or.model.clone().unwrap()));
        assert!(or.headers.contains_key("X-Title"));
    }

    #[test]
    fn duplicate_ids_rejected() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(provider("a"));
        cfg.providers.push(provider("a"));
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn default_provider_must_exist() {
        let cfg = AppConfig {
            default_provider: Some("ghost".into()),
            ..Default::default()
        };
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn openai_compatible_requires_endpoint() {
        let mut p = provider("c");
        p.protocol = ProtocolKind::OpenAiCompatible;
        assert!(p.validate().is_err());
        p.endpoint = Some(EndpointTemplate {
            chat_path: "/chat".into(),
            models_path: None,
            method: "POST".into(),
            request: RequestTemplate::default(),
            response: ResponseMapping {
                text_path: "output.text".into(),
                ..Default::default()
            },
            stream: StreamMapping::None,
            capabilities: Default::default(),
        });
        assert!(p.validate().is_ok());
    }

    #[test]
    fn config_roundtrips_without_secrets() {
        let mut cfg = AppConfig::default();
        cfg.providers.push(provider("openai"));
        let json = cfg.to_json_pretty().unwrap();
        assert!(json.contains("secret_ref"));
        assert!(!json.to_lowercase().contains("sk-"));
        let back = AppConfig::from_json(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn bearer_auth_sets_header() {
        let req = PreparedRequest::new(
            reqwest::Method::POST,
            url::Url::parse("https://x/v1").unwrap(),
            "t",
        );
        let req = apply_auth(req, &AuthConfig::Bearer, Some("sk-secret")).unwrap();
        assert!(req
            .headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer sk-secret"));
    }

    #[test]
    fn missing_secret_is_auth_error() {
        let req = PreparedRequest::new(
            reqwest::Method::POST,
            url::Url::parse("https://x/v1").unwrap(),
            "t",
        );
        let err = apply_auth(req, &AuthConfig::Bearer, None).unwrap_err();
        assert_eq!(err.kind(), "auth");
    }

    #[test]
    fn query_param_auth_appends_to_url() {
        let req = PreparedRequest::new(
            reqwest::Method::GET,
            url::Url::parse("https://x/v1").unwrap(),
            "t",
        );
        let req = apply_auth(req, &AuthConfig::QueryParam { name: "key".into() }, Some("abc")).unwrap();
        assert!(req.url.as_str().contains("key=abc"));
    }
}
