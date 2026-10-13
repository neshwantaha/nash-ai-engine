//! Secret resolution interface.
//!
//! The core never depends on a concrete secret backend. `nash-store` provides
//! an OS-keyring implementation; tests and the CLI can use
//! [`EnvSecretResolver`]. Secrets are fetched by *name* (an opaque reference),
//! never embedded in configuration.

use crate::error::Result;

/// Resolves a secret reference to its value.
pub trait SecretResolver: Send + Sync {
    /// Return `Ok(None)` when no secret is stored under `name`.
    fn resolve(&self, name: &str) -> Result<Option<String>>;
}

/// Reads secrets from `NASH_SECRET_<UPPER_NAME>` environment variables. Useful
/// for CI, containers and headless hosts where no OS keyring is available.
#[derive(Debug, Default, Clone)]
pub struct EnvSecretResolver;

impl EnvSecretResolver {
    pub fn new() -> Self {
        Self
    }
}

fn env_var_name(name: &str) -> String {
    format!(
        "NASH_SECRET_{}",
        name.to_ascii_uppercase().replace(['-', '.'], "_")
    )
}

impl SecretResolver for EnvSecretResolver {
    fn resolve(&self, name: &str) -> Result<Option<String>> {
        Ok(std::env::var(env_var_name(name)).ok().filter(|s| !s.is_empty()))
    }
}

/// Chains multiple resolvers: the first that yields a value wins.
#[derive(Default)]
pub struct ChainedResolver {
    resolvers: Vec<Box<dyn SecretResolver>>,
}

impl ChainedResolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, r: Box<dyn SecretResolver>) -> Self {
        self.resolvers.push(r);
        self
    }
}

impl SecretResolver for ChainedResolver {
    fn resolve(&self, name: &str) -> Result<Option<String>> {
        for r in &self.resolvers {
            if let Some(v) = r.resolve(name)? {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_resolver_reads_prefixed_var() {
        std::env::set_var("NASH_SECRET_TEST_KEY_X", "value123");
        let r = EnvSecretResolver::new();
        assert_eq!(r.resolve("test-key_x").unwrap().as_deref(), Some("value123"));
        assert_eq!(r.resolve("absent").unwrap(), None);
        std::env::remove_var("NASH_SECRET_TEST_KEY_X");
    }

    #[test]
    fn chained_prefers_first_hit() {
        let chain = ChainedResolver::new()
            .with(Box::new(EnvSecretResolver::new()));
        std::env::set_var("NASH_SECRET_CHAINED", "v");
        assert_eq!(chain.resolve("chained").unwrap().as_deref(), Some("v"));
        std::env::remove_var("NASH_SECRET_CHAINED");
    }
}
