//! Provider registry: an ordered, thread-safe map of live providers.

use crate::config::{AppConfig, ProviderConfig};
use crate::error::Result;
use crate::http::HttpClient;
use crate::provider::{Provider, ProviderInfo};
use crate::secrets::SecretResolver;
use indexmap::IndexMap;
use parking_lot::RwLock;
use std::sync::Arc;

/// Ordered collection of constructed providers.
#[derive(Default)]
pub struct ProviderRegistry {
    providers: RwLock<IndexMap<String, Arc<dyn Provider>>>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderRegistry")
            .field("ids", &self.ids())
            .finish()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert (or replace) a provider. Returns the replaced provider, if any.
    pub fn insert(&self, provider: Arc<dyn Provider>) -> Option<Arc<dyn Provider>> {
        self.providers
            .write()
            .insert(provider.id().to_string(), provider)
    }

    pub fn remove(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers.write().shift_remove(id)
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Provider>> {
        self.providers.read().get(id).cloned()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.providers.read().contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.providers.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.providers.read().is_empty()
    }

    pub fn ids(&self) -> Vec<String> {
        self.providers.read().keys().cloned().collect()
    }

    pub fn all(&self) -> Vec<Arc<dyn Provider>> {
        self.providers.read().values().cloned().collect()
    }

    pub fn infos(&self) -> Vec<ProviderInfo> {
        self.providers.read().values().map(|p| p.info()).collect()
    }
}

/// Construct every enabled provider in `config`.
///
/// * Each provider's referenced secret is resolved through `secrets`.
/// * A single provider that fails to build is reported and skipped, so one bad
///   entry cannot prevent the rest of the engine from starting.
pub fn build_registry(
    config: &AppConfig,
    http: &HttpClient,
    secrets: &dyn SecretResolver,
) -> Result<(ProviderRegistry, Vec<(String, String)>)> {
    let registry = ProviderRegistry::new();
    let mut errors = Vec::new();
    for cfg in config.providers.iter().filter(|p| p.enabled) {
        match build_provider(cfg, http, secrets) {
            Ok(p) => {
                registry.insert(p);
            }
            Err(e) => {
                tracing::warn!(provider = %cfg.id, error = %e, "failed to build provider");
                errors.push((cfg.id.clone(), e.to_string()));
            }
        }
    }
    Ok((registry, errors))
}

/// Build one provider from its configuration.
pub fn build_provider(
    cfg: &ProviderConfig,
    http: &HttpClient,
    secrets: &dyn SecretResolver,
) -> Result<Arc<dyn Provider>> {
    cfg.validate()?;
    let secret = match &cfg.secret_ref {
        Some(name) => secrets.resolve(name)?,
        None => None,
    };
    crate::adapters::build(cfg.clone(), secret, http.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::Capabilities;
    use crate::request::ChatRequest;
    use crate::stream::EventStream;
    use crate::provider::ModelInfo;
    use async_trait::async_trait;
    use tokio_util::sync::CancellationToken;

    #[derive(Debug)]
    struct Dummy(&'static str);

    #[async_trait]
    impl Provider for Dummy {
        fn id(&self) -> &str {
            self.0
        }
        fn info(&self) -> ProviderInfo {
            ProviderInfo {
                id: self.0.into(),
                display_name: self.0.into(),
                protocol: crate::config::ProtocolKind::OpenAiChat,
                category: crate::provider::ProviderCategory::Hosted,
                base_url: "https://x".into(),
                local: false,
                requires_api_key: true,
                docs_url: None,
            }
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::unknown()
        }
        async fn list_models(&self, _c: &CancellationToken) -> Result<Vec<ModelInfo>> {
            Ok(vec![])
        }
        async fn stream(&self, _r: ChatRequest, _c: CancellationToken) -> Result<EventStream> {
            unimplemented!()
        }
    }

    #[test]
    fn insert_get_remove_preserves_order() {
        let reg = ProviderRegistry::new();
        reg.insert(Arc::new(Dummy("a")));
        reg.insert(Arc::new(Dummy("b")));
        assert_eq!(reg.ids(), vec!["a", "b"]);
        assert!(reg.contains("a"));
        reg.remove("a");
        assert_eq!(reg.ids(), vec!["b"]);
    }
}
