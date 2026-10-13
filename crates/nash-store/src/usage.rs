//! Usage accounting: one row per completed request.

use crate::db::Store;
use nash_core::error::{EngineError, Result};
use rusqlite::{params, Row};
use serde::{Deserialize, Serialize};

/// A single request's token/latency accounting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    /// Set on read; ignored on write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
    pub latency_ms: u64,
    pub streaming: bool,
    pub created_at: String,
}

impl UsageRecord {
    /// A record for now, with `total_tokens` filled if it was left at zero.
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Self {
        let mut r = Self {
            id: None,
            session_id: None,
            provider: provider.into(),
            model: model.into(),
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            latency_ms: 0,
            streaming: false,
            created_at: nash_core::now_rfc3339(),
        };
        r.normalise();
        r
    }

    fn normalise(&mut self) {
        if self.total_tokens == 0 {
            self.total_tokens = self.prompt_tokens + self.completion_tokens;
        }
    }
}

/// Aggregated usage for a session or provider.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageTotals {
    pub requests: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub latency_ms: u64,
}

impl Store {
    /// Insert a usage record, returning its row id.
    pub fn record_usage(&self, record: &UsageRecord) -> Result<i64> {
        let mut r = record.clone();
        r.normalise();
        self.with_conn(|c| {
            c.execute(
                "INSERT INTO usage
                    (session_id, provider, model, prompt_tokens, completion_tokens, total_tokens, latency_ms, streaming, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    r.session_id,
                    r.provider,
                    r.model,
                    r.prompt_tokens,
                    r.completion_tokens,
                    r.total_tokens,
                    r.latency_ms,
                    r.streaming as i64,
                    r.created_at,
                ],
            )
            .map_err(|e| EngineError::Other(format!("database error: {e}")))?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Totals across a session.
    pub fn session_usage(&self, session_id: &str) -> Result<UsageTotals> {
        self.with_conn(|c| {
            c.query_row(
                "SELECT COUNT(*), COALESCE(SUM(prompt_tokens),0), COALESCE(SUM(completion_tokens),0),
                        COALESCE(SUM(total_tokens),0), COALESCE(SUM(latency_ms),0)
                 FROM usage WHERE session_id=?1",
                params![session_id],
                row_to_totals,
            )
            .map_err(|e| EngineError::Other(format!("database error: {e}")))
        })
    }

    /// Totals grouped by provider, most-used first.
    pub fn usage_by_provider(&self) -> Result<Vec<(String, UsageTotals)>> {
        self.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT provider, COUNT(*), COALESCE(SUM(prompt_tokens),0),
                            COALESCE(SUM(completion_tokens),0), COALESCE(SUM(total_tokens),0),
                            COALESCE(SUM(latency_ms),0)
                     FROM usage GROUP BY provider ORDER BY SUM(total_tokens) DESC",
                )
                .map_err(|e| EngineError::Other(format!("database error: {e}")))?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row_to_totals_at(row, 1)?))
                })
                .map_err(|e| EngineError::Other(format!("database error: {e}")))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(|e| EngineError::Other(format!("database error: {e}")))
        })
    }
}

fn row_to_totals(row: &Row<'_>) -> rusqlite::Result<UsageTotals> {
    row_to_totals_at(row, 0)
}

fn row_to_totals_at(row: &Row<'_>, base: usize) -> rusqlite::Result<UsageTotals> {
    Ok(UsageTotals {
        requests: row.get::<_, i64>(base)? as u64,
        prompt_tokens: row.get::<_, i64>(base + 1)? as u64,
        completion_tokens: row.get::<_, i64>(base + 2)? as u64,
        total_tokens: row.get::<_, i64>(base + 3)? as u64,
        latency_ms: row.get::<_, i64>(base + 4)? as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_and_aggregate() {
        let store = Store::open_in_memory().unwrap();
        let mut a = UsageRecord::new("openai", "gpt-4o");
        a.prompt_tokens = 10;
        a.completion_tokens = 5;
        a.session_id = Some("s1".into());
        a.latency_ms = 120;
        Store::record_usage(&store, &a).unwrap();
        Store::record_usage(&store, &a).unwrap();

        let totals = store.session_usage("s1").unwrap();
        assert_eq!(totals.requests, 2);
        assert_eq!(totals.prompt_tokens, 20);
        assert_eq!(totals.completion_tokens, 10);
        assert_eq!(totals.total_tokens, 30);
        assert_eq!(totals.latency_ms, 240);

        let by = store.usage_by_provider().unwrap();
        assert_eq!(by.len(), 1);
        assert_eq!(by[0].0, "openai");
        assert_eq!(by[0].1.requests, 2);
    }
}
