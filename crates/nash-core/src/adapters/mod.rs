//! Protocol adapters.
//!
//! Each adapter translates between the engine's uniform model ([`crate::request`],
//! [`crate::response`], [`crate::stream`]) and one provider wire protocol.
//! [`build`] is the single factory the registry uses, so adding a protocol means
//! adding a module here and one arm to the match below.

pub mod anthropic;
pub mod common;
pub mod custom;
pub mod gemini;
pub mod ollama;
pub mod openai_chat;
pub mod openai_responses;
pub mod rest;

pub use custom::CustomEndpointProvider;
pub use rest::{RestClient, RestRequest, RestResponse};

use crate::config::{ProtocolKind, ProviderConfig};
use crate::error::{EngineError, Result};
use crate::http::HttpClient;
use crate::provider::Provider;
use std::sync::Arc;

use anthropic::AnthropicProvider;
use gemini::GeminiProvider;
use ollama::OllamaProvider;
use openai_chat::OpenAiChatProvider;
use openai_responses::OpenAiResponsesProvider;

/// Construct the adapter for `cfg`'s protocol.
///
/// `secret` is the already-resolved credential (or `None`). It is never logged.
/// `CustomRest` is intentionally rejected here: a generic REST endpoint is not a
/// model endpoint and must go through [`RestClient`] instead.
pub fn build(
    cfg: ProviderConfig,
    secret: Option<String>,
    http: HttpClient,
) -> Result<Arc<dyn Provider>> {
    let provider: Arc<dyn Provider> = match cfg.protocol {
        ProtocolKind::OpenAiChat => Arc::new(OpenAiChatProvider::new(cfg, secret, http)?),
        ProtocolKind::OpenAiResponses => {
            Arc::new(OpenAiResponsesProvider::new(cfg, secret, http)?)
        }
        ProtocolKind::AnthropicMessages => Arc::new(AnthropicProvider::new(cfg, secret, http)?),
        ProtocolKind::GoogleGemini => Arc::new(GeminiProvider::new(cfg, secret, http)?),
        ProtocolKind::Ollama => Arc::new(OllamaProvider::new(cfg, secret, http)?),
        ProtocolKind::OpenAiCompatible => Arc::new(CustomEndpointProvider::new(cfg, secret, http)?),
        ProtocolKind::CustomRest => {
            return Err(EngineError::Config(format!(
                "provider '{}' is a generic REST endpoint, not a model endpoint; \
                 use the REST client instead of the provider registry",
                cfg.id
            )));
        }
    };
    Ok(provider)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuthConfig, RetryConfig};
    use std::collections::BTreeMap;

    fn cfg(id: &str, protocol: ProtocolKind) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            display_name: None,
            protocol,
            base_url: "https://api.example.com/v1".into(),
            auth: AuthConfig::Bearer,
            secret_ref: Some("k".into()),
            headers: BTreeMap::new(),
            query: BTreeMap::new(),
            model: Some("test-model".into()),
            models: vec![],
            timeout_ms: None,
            retry: RetryConfig::default(),
            concurrency: None,
            capabilities: Default::default(),
            endpoint: None,
            local: false,
            tags: vec![],
            enabled: true,
        }
    }

    #[test]
    fn builds_every_inference_protocol() {
        let http = HttpClient::new(Default::default()).unwrap();
        for protocol in [
            ProtocolKind::OpenAiChat,
            ProtocolKind::OpenAiResponses,
            ProtocolKind::AnthropicMessages,
            ProtocolKind::GoogleGemini,
            ProtocolKind::Ollama,
        ] {
            let p = build(cfg("x", protocol), Some("secret".into()), http.clone())
                .unwrap_or_else(|e| panic!("{protocol:?} failed: {e}"));
            assert_eq!(p.id(), "x");
        }
    }

    #[test]
    fn custom_rest_is_rejected() {
        let http = HttpClient::new(Default::default()).unwrap();
        let err = build(cfg("r", ProtocolKind::CustomRest), None, http).unwrap_err();
        assert_eq!(err.kind(), "config");
    }
}
