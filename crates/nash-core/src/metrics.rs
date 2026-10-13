//! Per-provider usage metrics.
//!
//! The engine records a sparse, in-memory account of what each provider has
//! done: request counts, latency, streamed event counts, token usage and the
//! error classes observed. Frontends use this for cost-awareness and the
//! connections/diagnostics view; nothing here is persisted.

use crate::stream::StreamEvent;
use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Instant;

/// Rolling counters for a single provider.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProviderMetrics {
    pub requests: u64,
    pub errors: u64,
    pub cancellations: u64,
    pub events_streamed: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_latency_ms: u64,
}

impl ProviderMetrics {
    /// Average request latency in milliseconds, or 0 with no samples.
    pub fn avg_latency_ms(&self) -> u64 {
        self.total_latency_ms
            .checked_div(self.requests)
            .unwrap_or(0)
    }
}

/// Thread-safe aggregation of metrics, keyed by provider id. Clone is an
/// `Arc` bump so a handle can be moved into a wrapped stream.
#[derive(Debug, Clone, Default)]
pub struct Metrics {
    inner: std::sync::Arc<RwLock<HashMap<String, ProviderMetrics>>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    fn with_metrics(&self, provider: &str, f: impl FnOnce(&mut ProviderMetrics)) {
        let mut map = self.inner.write().expect("metrics lock poisoned");
        let m = map.entry(provider.to_string()).or_default();
        f(m);
    }

    /// Record a completed (non-streamed) request that produced `usage`.
    pub fn record_response(&self, provider: &str, started: Instant, usage: &crate::response::Usage) {
        self.with_metrics(provider, |m| {
            m.requests += 1;
            m.total_latency_ms += started.elapsed().as_millis() as u64;
            m.prompt_tokens += usage.prompt_tokens as u64;
            m.completion_tokens += usage.completion_tokens as u64;
        });
    }

    /// Record failure of a request to complete.
    pub fn record_error(&self, provider: &str) {
        self.with_metrics(provider, |m| m.errors += 1);
    }

    /// Record an aborted/cancelled request.
    pub fn record_cancellation(&self, provider: &str) {
        self.with_metrics(provider, |m| m.cancellations += 1);
    }

    /// Record one consumed stream event plus any token counts it carried.
    pub fn record_stream_event(&self, provider: &str, ev: &StreamEvent) {
        self.with_metrics(provider, |m| {
            m.events_streamed += 1;
            if let StreamEvent::Usage(u) = ev {
                m.prompt_tokens += u.prompt_tokens as u64;
                m.completion_tokens += u.completion_tokens as u64;
            }
        });
    }

    /// Snapshot of all providers' counters.
    pub fn snapshot(&self) -> HashMap<String, ProviderMetrics> {
        self.inner
            .read()
            .expect("metrics lock poisoned")
            .clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response::Usage;

    #[test]
    fn records_response_counts_and_latency() {
        let m = Metrics::new();
        let start = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        m.record_response("openai", start, &Usage::new(10, 5));
        let snap = m.snapshot();
        let pm = &snap["openai"];
        assert_eq!(pm.requests, 1);
        assert_eq!(pm.prompt_tokens, 10);
        assert_eq!(pm.completion_tokens, 5);
        assert!(pm.total_latency_ms >= 1);
        assert!(pm.avg_latency_ms() >= 1);
    }

    #[test]
    fn separates_error_and_success() {
        let m = Metrics::new();
        m.record_error("anthropic");
        m.record_cancellation("ollama");
        let snap = m.snapshot();
        assert_eq!(snap["anthropic"].errors, 1);
        assert_eq!(snap["ollama"].cancellations, 1);
        assert_eq!(snap["anthropic"].requests, 0);
        assert_eq!(snap["anthropic"].total_latency_ms, 0);
    }

    #[test]
    fn absent_providers_default_to_zero() {
        let m = Metrics::new();
        assert_eq!(
            m.snapshot().get("nope").unwrap_or(&ProviderMetrics::default()),
            &ProviderMetrics::default()
        );
    }
}