//! Unified error type for the Nash engine.
//!
//! Every error surfaced by a provider adapter is normalised into [`EngineError`]
//! so the CLI, the agent and the desktop UI can react consistently (retry,
//! surface a diagnostic, ask the user for a new credential, ...).

use std::time::Duration;

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, EngineError>;

/// A normalised engine error.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    /// The provider configuration itself is invalid (missing base URL, bad
    /// template, unknown protocol, ...).
    #[error("invalid configuration: {0}")]
    Config(String),

    /// Credentials are missing or rejected.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// The credential is valid but is not allowed to perform the operation.
    #[error("permission denied: {0}")]
    Permission(String),

    /// The upstream API returned a non-success status code.
    #[error("provider API error ({status}): {message}")]
    Api {
        status: u16,
        message: String,
        /// Sanitised (redacted) response body, when useful for debugging.
        body: Option<String>,
        /// Whether the engine considers this failure safe to retry.
        retryable: bool,
    },

    /// The provider signalled rate limiting. `retry_after_ms` comes from the
    /// `Retry-After` header when present.
    #[error("rate limited{}", .retry_after_ms.map(|m| format!(" (retry after {m} ms)")).unwrap_or_default())]
    RateLimited { retry_after_ms: Option<u64> },

    /// A transport-level failure (DNS, TLS, connection reset, ...).
    #[error("network error: {0}")]
    Network(String),

    /// The request exceeded its configured timeout.
    #[error("request timed out after {0:?}")]
    Timeout(Duration),

    /// The operation was cancelled by the caller.
    #[error("request cancelled")]
    Cancelled,

    /// The provider or model does not support the requested capability.
    #[error("unsupported capability: {0}")]
    Unsupported(String),

    /// The provider response could not be decoded.
    #[error("failed to decode provider response: {0}")]
    Decode(String),

    /// The request was blocked by a local security policy (SSRF, plaintext
    /// remote endpoint, ...).
    #[error("blocked by security policy: {0}")]
    Security(String),

    /// A tool invocation failed.
    #[error("tool error: {0}")]
    Tool(String),

    /// An action was refused by the local approval gate (not by the provider).
    #[error("denied: {0}")]
    Denied(String),

    /// No provider in the registry could satisfy a routing request.
    #[error("no provider available: {0}")]
    NoProvider(String),

    /// Underlying I/O failure.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON (de)serialisation failure.
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    /// A catch-all for anything that does not fit the categories above.
    #[error("{0}")]
    Other(String),
}

impl EngineError {
    /// Whether a retry could plausibly succeed without changing the request.
    ///
    /// Authentication, permission, configuration and decode errors are *not*
    /// retryable: retrying them only wastes quota. Rate limits, transient
    /// network failures, timeouts and 5xx responses are.
    pub fn is_retryable(&self) -> bool {
        match self {
            EngineError::RateLimited { .. }
            | EngineError::Network(_)
            | EngineError::Timeout(_) => true,
            EngineError::Api { retryable, .. } => *retryable,
            _ => false,
        }
    }

    /// The HTTP status associated with the error, when any.
    pub fn status(&self) -> Option<u16> {
        match self {
            EngineError::Api { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// A stable, machine-readable discriminant (used by the UI and tests).
    pub fn kind(&self) -> &'static str {
        match self {
            EngineError::Config(_) => "config",
            EngineError::Auth(_) => "auth",
            EngineError::Permission(_) => "permission",
            EngineError::Api { .. } => "api",
            EngineError::RateLimited { .. } => "rate_limited",
            EngineError::Network(_) => "network",
            EngineError::Timeout(_) => "timeout",
            EngineError::Cancelled => "cancelled",
            EngineError::Unsupported(_) => "unsupported",
            EngineError::Decode(_) => "decode",
            EngineError::Security(_) => "security",
            EngineError::Tool(_) => "tool",
            EngineError::Denied(_) => "denied",
            EngineError::NoProvider(_) => "no_provider",
            EngineError::Io(_) => "io",
            EngineError::Json(_) => "json",
            EngineError::Other(_) => "other",
        }
    }

    /// Map an HTTP status and (already parsed) provider error message into a
    /// normalised error. `retry_after` is the parsed `Retry-After` header.
    pub fn from_http(status: u16, message: String, body: Option<String>, retry_after_ms: Option<u64>) -> Self {
        match status {
            401 => EngineError::Auth(message),
            403 => EngineError::Permission(message),
            408 | 425 | 429 => EngineError::RateLimited { retry_after_ms },
            400 | 404 | 405 | 409 | 413 | 415 | 422 => EngineError::Api {
                status,
                message,
                body,
                retryable: false,
            },
            500..=599 => EngineError::Api {
                status,
                message,
                body,
                retryable: true,
            },
            _ => EngineError::Api {
                status,
                message,
                body,
                retryable: false,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryability_is_conservative() {
        assert!(EngineError::Network("reset".into()).is_retryable());
        assert!(EngineError::RateLimited { retry_after_ms: None }.is_retryable());
        assert!(EngineError::Api {
            status: 503,
            message: "busy".into(),
            body: None,
            retryable: true
        }
        .is_retryable());

        assert!(!EngineError::Auth("bad key".into()).is_retryable());
        assert!(!EngineError::Config("nope".into()).is_retryable());
        assert!(!EngineError::Unsupported("tools".into()).is_retryable());
    }

    #[test]
    fn http_status_classifies_correctly() {
        assert!(matches!(
            EngineError::from_http(401, "no".into(), None, None),
            EngineError::Auth(_)
        ));
        assert!(matches!(
            EngineError::from_http(429, "slow".into(), None, Some(1500)),
            EngineError::RateLimited {
                retry_after_ms: Some(1500)
            }
        ));
        assert!(matches!(
            EngineError::from_http(404, "gone".into(), None, None),
            EngineError::Api {
                retryable: false,
                ..
            }
        ));
        assert!(matches!(
            EngineError::from_http(502, "bad gw".into(), None, None),
            EngineError::Api {
                retryable: true,
                ..
            }
        ));
    }

    #[test]
    fn kind_is_stable() {
        assert_eq!(EngineError::Cancelled.kind(), "cancelled");
        assert_eq!(EngineError::Auth("x".into()).kind(), "auth");
    }
}
