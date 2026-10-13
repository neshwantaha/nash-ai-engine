//! Retry policy with exponential backoff, jitter and `Retry-After` support.

use std::time::Duration;

/// Configuration for automatic retries of transient failures.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Total attempts including the first (1 = no retry).
    pub max_attempts: u32,
    /// Base backoff delay.
    pub base_delay: Duration,
    /// Upper bound on a single backoff delay.
    pub max_delay: Duration,
    /// Apply full jitter to backoff delays (recommended).
    pub jitter: bool,
    /// Honour the `Retry-After` response header when present.
    pub respect_retry_after: bool,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            jitter: true,
            respect_retry_after: true,
        }
    }
}

impl RetryPolicy {
    /// Disable retries entirely.
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Default::default()
        }
    }

    pub fn with_max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n.max(1);
        self
    }

    /// The backoff delay before retry number `attempt` (1-based: the delay
    /// after the first failure is `attempt == 1`).
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        if attempt == 0 {
            return Duration::ZERO;
        }
        let exp = self
            .base_delay
            .saturating_mul(1u32.checked_shl(attempt - 1).unwrap_or(u32::MAX));
        let capped = exp.min(self.max_delay);
        if self.jitter {
            // Full jitter in [capped/2, capped] keeps the mean sane while
            // de-synchronising concurrent clients.
            let half = capped / 2;
            let extra_ms = capped.saturating_sub(half).as_millis() as u64;
            let r = if extra_ms == 0 {
                0
            } else {
                rand::random::<u64>() % (extra_ms + 1)
            };
            half + Duration::from_millis(r)
        } else {
            capped
        }
    }
}

/// Parse a `Retry-After` header value. Supports delta-seconds. HTTP-date values
/// are treated as unknown (`None`) rather than guessed.
pub fn parse_retry_after(value: &str) -> Option<u64> {
    let v = value.trim();
    if let Ok(secs) = v.parse::<u64>() {
        return Some(secs.saturating_mul(1000));
    }
    if let Ok(f) = v.parse::<f64>() {
        if f.is_finite() && f >= 0.0 {
            return Some((f * 1000.0) as u64);
        }
    }
    None
}

/// Whether an HTTP status should be retried before the body is consumed.
pub fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504 | 507 | 522 | 524)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponential_backoff_is_capped() {
        let p = RetryPolicy {
            jitter: false,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(1000),
            ..Default::default()
        };
        assert_eq!(p.delay_for_attempt(0), Duration::ZERO);
        assert_eq!(p.delay_for_attempt(1), Duration::from_millis(100));
        assert_eq!(p.delay_for_attempt(2), Duration::from_millis(200));
        assert_eq!(p.delay_for_attempt(3), Duration::from_millis(400));
        assert_eq!(p.delay_for_attempt(10), Duration::from_millis(1000));
    }

    #[test]
    fn jitter_stays_within_bounds() {
        let p = RetryPolicy {
            jitter: true,
            base_delay: Duration::from_millis(1000),
            max_delay: Duration::from_secs(10),
            ..Default::default()
        };
        for _ in 0..50 {
            let d = p.delay_for_attempt(1);
            assert!(d >= Duration::from_millis(500) && d <= Duration::from_millis(1000));
        }
    }

    #[test]
    fn retry_after_parsing() {
        assert_eq!(parse_retry_after("2"), Some(2000));
        assert_eq!(parse_retry_after("1.5"), Some(1500));
        assert_eq!(parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"), None);
    }

    #[test]
    fn retryable_statuses() {
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(503));
        assert!(!is_retryable_status(401));
        assert!(!is_retryable_status(404));
    }
}
