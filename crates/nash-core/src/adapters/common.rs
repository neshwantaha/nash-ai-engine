//! Shared helpers used by the adapters.

use crate::config::{AuthConfig, ProviderConfig};
use crate::error::{EngineError, Result};
use crate::message::{ChatMessage, ContentPart, Role, ToolCall, ToolChoice, ToolDefinition};
use crate::provider::{ProviderCategory, ProviderInfo};
use crate::security::redact_text;
use reqwest::header::HeaderMap;
use serde_json::{json, Value};
use url::Url;

/// Maximum number of bytes of an error body we will read/log.
pub const ERROR_BODY_LIMIT: usize = 8 * 1024;

/// Join a base URL with a path segment, tolerating trailing/leading slashes.
pub fn join_url(base: &str, path: &str) -> Result<Url> {
    let base = base.trim_end_matches('/');
    let path = path.trim_start_matches('/');
    let full = if path.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{path}")
    };
    Url::parse(&full).map_err(|e| EngineError::Config(format!("invalid URL '{full}': {e}")))
}

/// Apply configured static headers and query params to a URL/header list.
pub fn apply_static(cfg: &ProviderConfig, headers: &mut Vec<(String, String)>, url: &mut Url) {
    for (k, v) in &cfg.query {
        url.query_pairs_mut().append_pair(k, v);
    }
    for (k, v) in &cfg.headers {
        headers.push((k.clone(), v.clone()));
    }
}

/// Categorise a provider for the UI based on protocol/local flag.
pub fn category_for(cfg: &ProviderConfig) -> ProviderCategory {
    if cfg.protocol == crate::config::ProtocolKind::OpenAiCompatible {
        return ProviderCategory::Custom;
    }
    if cfg.local {
        return ProviderCategory::Local;
    }
    match cfg.id.as_str() {
        "openrouter" | "together" | "fireworks" | "deepinfra" => ProviderCategory::Aggregator,
        "vllm" | "lmstudio" | "llamacpp" | "text-generation-webui" => ProviderCategory::SelfHosted,
        _ => ProviderCategory::Hosted,
    }
}

/// Build a [`ProviderInfo`] for a config.
pub fn provider_info(cfg: &ProviderConfig, default_display: &str, docs_url: Option<&str>) -> ProviderInfo {
    ProviderInfo {
        id: cfg.id.clone(),
        display_name: cfg
            .display_name
            .clone()
            .unwrap_or_else(|| default_display.to_string()),
        protocol: cfg.protocol,
        category: category_for(cfg),
        base_url: cfg.base_url.clone(),
        local: cfg.local,
        requires_api_key: cfg.auth != AuthConfig::None,
        docs_url: docs_url.map(str::to_string),
    }
}

/// Read at most `limit` bytes from a response body.
pub async fn read_body_limited(resp: reqwest::Response, limit: usize) -> (u16, HeaderMap, String) {
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let text = match resp.text().await {
        Ok(t) => {
            if t.len() > limit {
                t.chars().take(limit).collect()
            } else {
                t
            }
        }
        Err(e) => format!("<failed to read body: {e}>"),
    };
    (status, headers, text)
}

/// Extract a human-readable message from a provider error body.
///
/// Every extraction path is routed through [`redact_text`] so a provider that
/// echoes an API key back inside an error message cannot leak it.
pub fn extract_error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        // { "error": { "message": "..." } }
        if let Some(m) = v
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            return redact_text(m);
        }
        // { "error": "..." }
        if let Some(m) = v.get("error").and_then(Value::as_str) {
            return redact_text(m);
        }
        // { "message": "..." }
        if let Some(m) = v.get("message").and_then(Value::as_str) {
            return redact_text(m);
        }
        // { "detail": "..." }
        if let Some(m) = v.get("detail").and_then(Value::as_str) {
            return redact_text(m);
        }
        // Anthropic: { "error": { "error": { "message": } } }
        if let Some(m) = v
            .get("error")
            .and_then(|e| e.get("error"))
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
        {
            return redact_text(m);
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "empty response body".to_string()
    } else {
        redact_text(trimmed)
    }
}

/// Build a normalised error from an HTTP response.
pub fn api_error(status: u16, headers: &HeaderMap, body: &str) -> EngineError {
    let retry_after = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(crate::http::parse_retry_after);
    let message = extract_error_message(body);
    let safe_body = if body.is_empty() {
        None
    } else {
        Some(redact_text(body))
    };
    EngineError::from_http(status, message, safe_body, retry_after)
}

/// Convert a chat message into the OpenAI Chat Completions wire shape.
pub fn openai_message(m: &ChatMessage) -> Value {
    let role = m.role.as_str();
    let mut obj = serde_json::Map::new();
    obj.insert("role".into(), json!(role));

    if m.has_images() {
        let mut parts = Vec::new();
        for p in &m.content {
            match p {
                ContentPart::Text { text } => {
                    parts.push(json!({"type": "text", "text": text}));
                }
                ContentPart::ImageUrl { url, detail } => {
                    let mut img = json!({ "url": url });
                    if let Some(d) = detail {
                        img["detail"] = json!(d);
                    }
                    parts.push(json!({"type": "image_url", "image_url": img}));
                }
            }
        }
        obj.insert("content".into(), Value::Array(parts));
    } else {
        obj.insert("content".into(), json!(m.text()));
    }

    if !m.tool_calls.is_empty() {
        let calls: Vec<Value> = m
            .tool_calls
            .iter()
            .map(|tc| {
                json!({
                    "id": tc.id,
                    "type": "function",
                    "function": {
                        "name": tc.name,
                        "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into())
                    }
                })
            })
            .collect();
        obj.insert("tool_calls".into(), Value::Array(calls));
    }
    if let Some(id) = &m.tool_call_id {
        if m.role == Role::Tool {
            obj.insert("tool_call_id".into(), json!(id));
        }
    }
    if let Some(name) = &m.name {
        obj.insert("name".into(), json!(name));
    }
    Value::Object(obj)
}

pub fn openai_tools(tools: &[ToolDefinition]) -> Value {
    Value::Array(
        tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    }
                })
            })
            .collect(),
    )
}

pub fn openai_tool_choice(choice: &ToolChoice) -> Value {
    match choice {
        ToolChoice::Auto => json!("auto"),
        ToolChoice::None => json!("none"),
        ToolChoice::Required => json!("required"),
        ToolChoice::Specific(name) => json!({"type": "function", "function": {"name": name}}),
    }
}

/// Parse an OpenAI-style `tool_calls` array into [`ToolCall`]s.
pub fn parse_openai_tool_calls(value: &Value) -> Vec<ToolCall> {
    let mut out = Vec::new();
    if let Some(arr) = value.as_array() {
        for (i, tc) in arr.iter().enumerate() {
            let id = tc
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{i}"));
            let func = tc.get("function").cloned().unwrap_or(Value::Null);
            let name = func.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            if name.is_empty() {
                continue;
            }
            let args = func
                .get("arguments")
                .and_then(Value::as_str)
                .map(|s| serde_json::from_str(s).unwrap_or(Value::Null))
                .unwrap_or(Value::Null);
            out.push(ToolCall::new(id, name, args));
        }
    }
    out
}

/// Parse a `usage` object (OpenAI shape and common variants).
pub fn parse_openai_usage(value: &Value) -> Option<crate::response::Usage> {
    let obj = value.as_object()?;
    let prompt = obj
        .get("prompt_tokens")
        .or_else(|| obj.get("input_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let completion = obj
        .get("completion_tokens")
        .or_else(|| obj.get("output_tokens"))
        .and_then(Value::as_u64)
        .unwrap_or(0) as u32;
    let total = obj
        .get("total_tokens")
        .and_then(Value::as_u64)
        .unwrap_or((prompt + completion) as u64) as u32;
    let reasoning = obj
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_u64)
        .map(|v| v as u32);
    let cached = obj
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_u64)
        .map(|v| v as u32);
    Some(crate::response::Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: total,
        reasoning_tokens: reasoning,
        cached_tokens: cached,
    })
}

/// A reusable connection test: authentication via `list_models`, model
/// availability, then a cheap streaming generation probe. Adapters may override
/// [`crate::provider::Provider::validate`] with something more specific.
pub async fn standard_validation(
    provider: &dyn crate::provider::Provider,
    cancel: &tokio_util::sync::CancellationToken,
) -> Result<crate::provider::ValidationReport> {
    use crate::provider::{CheckResult, CheckStatus};
    use futures::StreamExt;
    use std::time::Instant;

    let mut report = crate::provider::ValidationReport::new(provider.id());

    let started = Instant::now();
    match provider.list_models(cancel).await {
        Ok(models) => {
            let ms = started.elapsed().as_millis() as u64;
            let detail = if models.is_empty() {
                "reachable; no models advertised".to_string()
            } else {
                format!("reachable; {} model(s)", models.len())
            };
            report.push(CheckResult::pass("authentication", Some(detail)).with_latency(ms));
            match provider.default_model() {
                Some(m) if !models.iter().any(|x| x.id == m) => report.push(
                    CheckResult::warn(
                        "model_availability",
                        format!("configured model '{m}' not in the listing"),
                    ),
                ),
                Some(m) => report.push(CheckResult::pass(
                    "model_availability",
                    Some(format!("'{m}' available")),
                )),
                None => report.push(
                    CheckResult::skip("model_availability", "no model configured"),
                ),
            };
        }
        Err(e) if e.kind() == "auth" || e.kind() == "permission" => {
            report.push(CheckResult::fail("authentication", e.to_string()));
            report.push(CheckResult::skip("model_availability", "authentication failed"));
            report.push(CheckResult::skip("streaming_generation", "authentication failed"));
            return Ok(report);
        }
        Err(e) if e.kind() == "unsupported" => {
            report.push(CheckResult::warn(
                "authentication",
                "no model-listing endpoint; login not verified",
            ));
            report.push(CheckResult::skip("model_availability", "no listing endpoint"));
        }
        Err(e) => {
            report.push(CheckResult::fail("authentication", e.to_string()));
            report.push(CheckResult::skip("model_availability", "connection failed"));
            report.push(CheckResult::skip("streaming_generation", "connection failed"));
            return Ok(report);
        }
    }

    match provider.default_model() {
        None => {
            report.push(CheckResult::skip(
                "streaming_generation",
                "no model configured to test generation",
            ));
        }
        Some(model) => {
            let probe = crate::request::ChatRequest::new(model)
                .message(crate::message::ChatMessage::user("ping"))
                .max_tokens(1);
            let started = Instant::now();
            match provider.stream(probe, cancel.clone()).await {
                Ok(mut s) => {
                    let mut ok = false;
                    let mut failure = None;
                    match s.next().await {
                        Some(Ok(_)) => ok = true,
                        Some(Err(e)) => failure = Some(e),
                        None => {}
                    }
                    let ms = started.elapsed().as_millis() as u64;
                    if ok {
                        report.push(
                            CheckResult::pass("streaming_generation", Some("stream open".into()))
                                .with_latency(ms),
                        );
                    } else if let Some(e) = failure {
                        report.push(CheckResult::fail("streaming_generation", e.to_string()));
                    } else {
                        report.push(CheckResult::fail(
                            "streaming_generation",
                            "no events received",
                        ));
                    }
                }
                Err(e) if e.kind() == "unsupported" => {
                    report.push(CheckResult::skip("streaming_generation", e.to_string()));
                }
                Err(e) => {
                    report.push(CheckResult::fail("streaming_generation", e.to_string()));
                }
            }
        }
    }

    let _ = CheckStatus::Pass;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_url_handles_slashes() {        assert_eq!(
            join_url("https://api.openai.com/v1", "/chat/completions")
                .unwrap()
                .as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            join_url("http://localhost:11434/", "api/chat").unwrap().as_str(),
            "http://localhost:11434/api/chat"
        );
    }

    #[test]
    fn extracts_error_messages_from_various_shapes() {
        assert_eq!(
            extract_error_message(r#"{"error":{"message":"bad"}}"#),
            "bad"
        );
        assert_eq!(extract_error_message(r#"{"message":"oops"}"#), "oops");
        assert_eq!(extract_error_message("plain text"), "plain text");
    }

    #[test]
    fn redacts_keys_in_error_bodies() {
        let msg = extract_error_message(r#"{"error":{"message":"invalid key sk-abcdefghijklmnop"}}"#);
        assert!(!msg.contains("abcdefghij"));
    }

    #[test]
    fn openai_message_text_only_is_string() {
        let v = openai_message(&ChatMessage::user("hi"));
        assert_eq!(v["role"], "user");
        assert_eq!(v["content"], "hi");
    }

    #[test]
    fn openai_message_with_image_is_array() {
        let m = ChatMessage::new(
            Role::User,
            vec![ContentPart::text("look"), ContentPart::image_url("http://x/y.png")],
        );
        let v = openai_message(&m);
        assert!(v["content"].is_array());
        assert_eq!(v["content"][1]["type"], "image_url");
    }

    #[test]
    fn openai_tool_call_serialisation() {
        let m = ChatMessage::assistant_tool_calls(vec![ToolCall::new(
            "call_1",
            "f",
            json!({"a": 1}),
        )]);
        let v = openai_message(&m);
        assert_eq!(v["tool_calls"][0]["function"]["name"], "f");
        let args = v["tool_calls"][0]["function"]["arguments"].as_str().unwrap();
        assert_eq!(serde_json::from_str::<Value>(args).unwrap()["a"], 1);
    }

    #[test]
    fn parses_usage_with_aliases() {
        let u = parse_openai_usage(&json!({"input_tokens": 3, "output_tokens": 4})).unwrap();
        assert_eq!(u.prompt_tokens, 3);
        assert_eq!(u.completion_tokens, 4);
        assert_eq!(u.total_tokens, 7);
    }

    #[test]
    fn api_error_respects_retry_after() {
        let mut h = HeaderMap::new();
        h.insert("retry-after", "3".parse().unwrap());
        let e = api_error(429, &h, r#"{"error":{"message":"slow"}}"#);
        assert!(matches!(e, EngineError::RateLimited { retry_after_ms: Some(3000) }));
    }
}
