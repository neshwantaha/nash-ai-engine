//! Credential and diagnostics redaction.
//!
//! Nothing in this crate should ever log or return a raw API key. All
//! diagnostic surfaces route through [`redact_secret`], [`redact_headers`]
//! and [`redact_json`].

use serde_json::Value;
use std::collections::BTreeMap;

/// Header names (lowercased) whose values must never be logged.
pub const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "x-auth-token",
    "cookie",
    "set-cookie",
];

/// JSON object keys (lowercased, matched by substring) whose values are
/// treated as secrets.
const SENSITIVE_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "api-key",
    "authorization",
    "auth",
    "token",
    "secret",
    "password",
    "passwd",
    "access_key",
    "private_key",
    "client_secret",
    "bearer",
];

/// Redact a secret for display. Keeps a short prefix/suffix for identification
/// but never reveals enough to reconstruct the credential.
pub fn redact_secret(secret: &str) -> String {
    let s = secret.trim();
    if s.is_empty() {
        return "«empty»".to_string();
    }
    let n = s.chars().count();
    if n <= 8 {
        return "«redacted»".to_string();
    }
    let prefix: String = s.chars().take(3).collect();
    let suffix: String = s.chars().rev().take(2).collect::<String>().chars().rev().collect();
    format!("{prefix}…{suffix}«redacted»")
}

/// True if a header name should be redacted.
pub fn is_sensitive_header(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SENSITIVE_HEADERS.contains(&lower.as_str())
}

/// True if a JSON key looks like it holds a secret.
pub fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    SENSITIVE_KEYS.iter().any(|k| lower.contains(k))
}

/// Return a copy of `headers` with sensitive values replaced.
pub fn redact_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(k, v)| {
            if is_sensitive_header(k) {
                (k.clone(), "«redacted»".to_string())
            } else {
                (k.clone(), v.clone())
            }
        })
        .collect()
}

/// Recursively redact secret-looking fields in a JSON value.
pub fn redact_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                if is_sensitive_key(k) {
                    out.insert(k.clone(), Value::String("«redacted»".to_string()));
                } else {
                    out.insert(k.clone(), redact_json(v));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_json).collect()),
        other => other.clone(),
    }
}

/// Redact obvious bearer/api-key material embedded in free-form text (e.g. a
/// provider error body that echoes the key). Best-effort and conservative.
pub fn redact_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for token in text.split_inclusive(|c: char| c.is_whitespace()) {
        let lower = token.to_ascii_lowercase();
        if (lower.starts_with("sk-") || lower.starts_with("sk_") || lower.starts_with("gsk_"))
            && token.trim().len() > 12
        {
            out.push_str("«redacted»");
            if token.ends_with(char::is_whitespace) {
                out.push(' ');
            }
        } else if lower.starts_with("bearer") {
            out.push_str("Bearer «redacted»");
            if token.ends_with(char::is_whitespace) {
                out.push(' ');
            }
        } else {
            out.push_str(token);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_secret_fully_hidden() {
        assert_eq!(redact_secret("abc"), "«redacted»");
        assert_eq!(redact_secret(""), "«empty»");
    }

    #[test]
    fn long_secret_partial() {
        let r = redact_secret("sk-abcdefghijklmnop");
        assert!(r.contains("redacted"));
        assert!(!r.contains("mnop"));
        assert!(r.starts_with("sk-"));
    }

    #[test]
    fn headers_are_redacted() {
        let mut h = BTreeMap::new();
        h.insert("Authorization".to_string(), "Bearer xyz".to_string());
        h.insert("X-Api-Key".to_string(), "abc".to_string());
        h.insert("Content-Type".to_string(), "application/json".to_string());
        let r = redact_headers(&h);
        assert_eq!(r["Authorization"], "«redacted»");
        assert_eq!(r["X-Api-Key"], "«redacted»");
        assert_eq!(r["Content-Type"], "application/json");
    }

    #[test]
    fn json_redaction_is_recursive() {
        let v = serde_json::json!({
            "model": "gpt-4o",
            "api_key": "sk-123",
            "nested": { "access_token": "abc", "ok": 1 },
            "list": [ { "password": "p" } ]
        });
        let r = redact_json(&v);
        assert_eq!(r["api_key"], "«redacted»");
        assert_eq!(r["nested"]["access_token"], "«redacted»");
        assert_eq!(r["nested"]["ok"], 1);
        assert_eq!(r["list"][0]["password"], "«redacted»");
        assert_eq!(r["model"], "gpt-4o");
    }

    #[test]
    fn text_redaction_catches_prefixed_keys() {
        let t = "error: invalid key sk-abcdefghijklmnopqrstuvwxyz given";
        let r = redact_text(t);
        assert!(!r.contains("abcdefghij"));
        assert!(r.contains("«redacted»"));
    }
}
