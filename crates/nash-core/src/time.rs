//! Small time helpers shared across crates.
//!
//! All persisted timestamps are RFC 3339 in UTC, which sorts lexicographically
//! and round-trips cleanly through SQLite `TEXT` columns.

use chrono::{DateTime, Utc};

/// Current UTC instant.
pub fn now() -> DateTime<Utc> {
    chrono::Utc::now()
}

/// Current UTC time as an RFC 3339 string.
pub fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Parse an RFC 3339 timestamp into a UTC instant.
pub fn parse_rfc3339(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.with_timezone(&chrono::Utc))
}
