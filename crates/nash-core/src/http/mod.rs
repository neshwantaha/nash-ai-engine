//! HTTP transport: pooled client, destination validation, retries and
//! streaming helpers.

pub mod ndjson;
pub mod retry;
pub mod sse;

use crate::error::{EngineError, Result};
use crate::security::SecurityPolicy;
use reqwest::redirect;
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

pub use ndjson::{ndjson_stream, NdjsonDecoder, NdjsonStream};
pub use retry::{is_retryable_status, parse_retry_after, RetryPolicy};
pub use sse::{sse_stream, SseEvent, SseStream};
/// Immutable HTTP client configuration.
#[derive(Debug, Clone)]
pub struct HttpClientConfig {
    /// Timeout applied to non-streaming requests.
    pub request_timeout: Duration,
    /// TCP/TLS connection timeout.
    pub connect_timeout: Duration,
    /// Keep-alive idle connections per host.
    pub pool_max_idle_per_host: usize,
    pub user_agent: String,
    pub security: Arc<SecurityPolicy>,
    pub retry: RetryPolicy,
}

impl Default for HttpClientConfig {
    fn default() -> Self {
        Self {
            request_timeout: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(15),
            pool_max_idle_per_host: 8,
            user_agent: format!("nash-ai-engine/{}", env!("CARGO_PKG_VERSION")),
            security: Arc::new(SecurityPolicy::default()),
            retry: RetryPolicy::default(),
        }
    }
}

/// A prepared, replayable outbound request. Replayability is what makes
/// bounded retry possible: the body is buffered once by the caller.
#[derive(Debug, Clone)]
pub struct PreparedRequest {
    pub method: reqwest::Method,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    /// Buffered body bytes (request bodies in this engine are small JSON).
    pub body: Option<Vec<u8>>,
    /// Per-request timeout override.
    pub timeout: Option<Duration>,
    /// Streaming requests skip the total timeout (they may run for minutes)
    /// and rely on the connect timeout plus cancellation.
    pub stream: bool,
    /// Short diagnostic label (never contains secrets).
    pub label: String,
}

impl PreparedRequest {
    pub fn new(method: reqwest::Method, url: Url, label: impl Into<String>) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            body: None,
            timeout: None,
            stream: false,
            label: label.into(),
        }
    }

    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn bearer(mut self, token: &str) -> Self {
        self.headers
            .push(("authorization".into(), format!("Bearer {token}")));
        self
    }

    pub fn json_body(mut self, value: &serde_json::Value) -> Result<Self> {
        let bytes = serde_json::to_vec(value)?;
        self.body = Some(bytes);
        self.headers
            .push(("content-type".into(), "application/json".into()));
        Ok(self)
    }

    pub fn streaming(mut self) -> Self {
        self.stream = true;
        self
    }

    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = Some(d);
        self
    }
}

/// Pooled HTTP client with retry and SSRF protection.
#[derive(Clone)]
pub struct HttpClient {
    client: reqwest::Client,
    cfg: Arc<HttpClientConfig>,
}

impl std::fmt::Debug for HttpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpClient")
            .field("request_timeout", &self.cfg.request_timeout)
            .finish_non_exhaustive()
    }
}

impl HttpClient {
    pub fn new(cfg: HttpClientConfig) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .user_agent(cfg.user_agent.clone())
            .connect_timeout(cfg.connect_timeout)
            .pool_max_idle_per_host(cfg.pool_max_idle_per_host)
            .pool_idle_timeout(Duration::from_secs(90))
            .tcp_keepalive(Duration::from_secs(30))
            // We never let a stale idle connection break a request.
            .http2_adaptive_window(true);

        let sec = cfg.security.clone();
        builder = builder.redirect(redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= 8 {
                return attempt.stop();
            }
            match sec.validate_url(attempt.url()) {
                Ok(()) => attempt.follow(),
                Err(e) => attempt.error(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    e.to_string(),
                )),
            }
        }));

        let client = builder
            .build()
            .map_err(|e| EngineError::Config(format!("failed to build HTTP client: {e}")))?;
        Ok(Self {
            client,
            cfg: Arc::new(cfg),
        })
    }

    pub fn config(&self) -> &HttpClientConfig {
        &self.cfg
    }

    pub fn security(&self) -> &SecurityPolicy {
        &self.cfg.security
    }

    pub fn retry_policy(&self) -> &RetryPolicy {
        &self.cfg.retry
    }

    /// Underlying reqwest client (for adapters that need fine control).
    pub fn raw(&self) -> &reqwest::Client {
        &self.client
    }

    /// Validate and execute `req`, retrying transient failures. Returns as soon
    /// as response headers are available; the body is not consumed, so this is
    /// safe to use before streaming.
    pub async fn execute(
        &self,
        req: PreparedRequest,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response> {
        // Static + DNS-level validation before any socket is opened.
        self.cfg.security.resolve_and_validate(&req.url).await?;

        let max = self.cfg.retry.max_attempts.max(1);
        let mut attempt: u32 = 1;
        loop {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }

            let mut rb = self.client.request(req.method.clone(), req.url.clone());
            for (k, v) in &req.headers {
                rb = rb.header(k.as_str(), v.as_str());
            }
            if let Some(body) = &req.body {
                rb = rb.body(body.clone());
            }
            let effective_timeout = if req.stream {
                None
            } else {
                Some(req.timeout.unwrap_or(self.cfg.request_timeout))
            };
            if let Some(t) = effective_timeout {
                rb = rb.timeout(t);
            }

            let send = rb.send();
            let outcome = tokio::select! {
                _ = cancel.cancelled() => return Err(EngineError::Cancelled),
                r = send => r,
            };

            match outcome {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if is_retryable_status(status) && attempt < max {
                        let retry_after = resp
                            .headers()
                            .get("retry-after")
                            .and_then(|v| v.to_str().ok())
                            .and_then(parse_retry_after);
                        tracing::debug!(
                            label = %req.label,
                            status,
                            attempt,
                            "retrying after transient status"
                        );
                        drop(resp);
                        let delay = self.backoff(attempt, retry_after);
                        sleep_cancellable(delay, cancel).await?;
                        attempt += 1;
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    let err = self.map_error(e);
                    if err.is_retryable() && attempt < max {
                        tracing::debug!(
                            label = %req.label,
                            attempt,
                            error = %redact_err(&err),
                            "retrying after transport error"
                        );
                        let delay = self.backoff(attempt, None);
                        sleep_cancellable(delay, cancel).await?;
                        attempt += 1;
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    fn backoff(&self, attempt: u32, retry_after_ms: Option<u64>) -> Duration {
        if self.cfg.retry.respect_retry_after {
            if let Some(ms) = retry_after_ms {
                return Duration::from_millis(ms).min(self.cfg.retry.max_delay);
            }
        }
        self.cfg.retry.delay_for_attempt(attempt)
    }

    fn map_error(&self, e: reqwest::Error) -> EngineError {
        let timeout = e
            .url()
            .map(|_| self.cfg.request_timeout)
            .unwrap_or(self.cfg.request_timeout);
        if e.is_timeout() {
            EngineError::Timeout(timeout)
        } else if e.is_connect() {
            EngineError::Network(format!("connection failed: {e}"))
        } else if e.is_decode() {
            EngineError::Decode(e.to_string())
        } else if e.is_redirect() {
            EngineError::Security(format!("redirect rejected: {e}"))
        } else {
            EngineError::Network(e.to_string())
        }
    }
}

async fn sleep_cancellable(d: Duration, cancel: &CancellationToken) -> Result<()> {
    tokio::select! {
        _ = cancel.cancelled() => Err(EngineError::Cancelled),
        _ = tokio::time::sleep(d) => Ok(()),
    }
}

fn redact_err(e: &EngineError) -> String {
    crate::security::redact_text(&e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_request_json_body_sets_content_type() {
        let req = PreparedRequest::new(
            reqwest::Method::POST,
            Url::parse("https://api.example.com/v1").unwrap(),
            "test",
        )
        .json_body(&serde_json::json!({"a": 1}))
        .unwrap();
        assert_eq!(req.body.as_deref(), Some(b"{\"a\":1}".as_ref()));
        assert!(req.headers.iter().any(|(k, _)| k == "content-type"));
    }

    #[test]
    fn client_builds_with_default_policy() {
        let c = HttpClient::new(HttpClientConfig::default());
        assert!(c.is_ok());
    }

    #[tokio::test]
    async fn blocks_metadata_before_sending() {
        let client = HttpClient::new(HttpClientConfig::default()).unwrap();
        let req = PreparedRequest::new(
            reqwest::Method::GET,
            Url::parse("http://169.254.169.254/latest/").unwrap(),
            "meta",
        );
        let cancel = CancellationToken::new();
        let err = client.execute(req, &cancel).await.unwrap_err();
        assert_eq!(err.kind(), "security");
    }
}
