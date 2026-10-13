//! Adapter for the Google Gemini API (`streamGenerateContent`).
//!
//! Gemini uses a `contents`/`parts` shape and puts the model in the URL path.

use super::common::*;
use crate::capability::{Capabilities, Support};
use crate::config::ProviderConfig;
use crate::error::{EngineError, Result};
use crate::http::{HttpClient, PreparedRequest};
use crate::message::{ContentPart, Role};
use crate::provider::{ModelInfo, Provider, ProviderInfo, ValidationReport};
use crate::request::{ChatRequest, ResponseFormat};
use crate::response::FinishReason;
use crate::stream::{EventStream, StreamEvent};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct GeminiProvider {
    cfg: ProviderConfig,
    secret: Option<String>,
    http: HttpClient,
    info: ProviderInfo,
    caps: Capabilities,
}

impl GeminiProvider {
    pub fn new(mut cfg: ProviderConfig, secret: Option<String>, http: HttpClient) -> Result<Self> {
        if cfg.base_url.trim().is_empty() {
            cfg.base_url = "https://generativelanguage.googleapis.com/v1beta".to_string();
        }
        let info = provider_info(&cfg, "Google Gemini", Some("https://ai.google.dev/api/generate-content"));
        let caps = Capabilities {
            streaming: Support::Supported,
            tools: Support::Supported,
            structured_output: Support::Supported,
            json_mode: Support::Supported,
            vision: Support::Supported,
            usage_reporting: Support::Supported,
            model_listing: Support::Supported,
            context_window: Some(1_000_000),
            ..Capabilities::default()
        }
        .overlay(&cfg.capabilities);
        Ok(Self {
            cfg,
            secret,
            http,
            info,
            caps,
        })
    }

    fn build_body(&self, req: &ChatRequest) -> Value {
        let mut system = String::new();
        let mut contents: Vec<Value> = Vec::new();
        for m in &req.messages {
            match m.role {
                Role::System => {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(&m.text());
                }
                Role::Tool => {
                    contents.push(json!({
                        "role": "user",
                        "parts": [{
                            "functionResponse": {
                                "name": m.name.clone().unwrap_or_else(|| "function".into()),
                                "response": {"content": m.text()}
                            }
                        }]
                    }));
                }
                Role::Assistant if !m.tool_calls.is_empty() => {
                    let parts: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|tc| json!({"functionCall": {"name": tc.name, "args": tc.arguments}}))
                        .collect();
                    contents.push(json!({"role": "model", "parts": parts}));
                }
                Role::Assistant => {
                    contents.push(json!({"role": "model", "parts": [{"text": m.text()}]}));
                }
                Role::User => {
                    let mut parts = Vec::new();
                    for p in &m.content {
                        match p {
                            ContentPart::Text { text } => parts.push(json!({"text": text})),
                            ContentPart::ImageUrl { url, .. } => {
                                if let Some((mime, data)) = parse_data_url(url) {
                                    parts.push(json!({
                                        "inlineData": {"mimeType": mime, "data": data}
                                    }));
                                }
                            }
                        }
                    }
                    contents.push(json!({"role": "user", "parts": parts}));
                }
            }
        }

        let mut gen = serde_json::Map::new();
        if let Some(t) = req.params.temperature {
            gen.insert("temperature".into(), json!(t));
        }
        if let Some(tp) = req.params.top_p {
            gen.insert("topP".into(), json!(tp));
        }
        if let Some(mx) = req.params.max_tokens {
            gen.insert("maxOutputTokens".into(), json!(mx));
        }
        if !req.params.stop.is_empty() {
            gen.insert("stopSequences".into(), json!(req.params.stop));
        }
        if matches!(req.params.response_format, ResponseFormat::JsonObject) {
            gen.insert("responseMimeType".into(), json!("application/json"));
        }

        let mut body = serde_json::Map::new();
        if !system.is_empty() {
            body.insert("systemInstruction".into(), json!({"parts": [{"text": system}]}));
        }
        body.insert("contents".into(), Value::Array(contents));
        if !gen.is_empty() {
            body.insert("generationConfig".into(), Value::Object(gen));
        }
        if req.wants_tools() {
            let decls: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": normalize_schema(&t.parameters)
                    })
                })
                .collect();
            body.insert("tools".into(), json!([{"functionDeclarations": decls}]));
        }
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }
        Value::Object(body)
    }

    fn prepare(&self, model: &str, body: &Value, stream: bool) -> Result<PreparedRequest> {
        let method = if stream {
            format!("models/{model}:streamGenerateContent")
        } else {
            format!("models/{model}:generateContent")
        };
        let mut url = join_url(&self.cfg.base_url, &method)?;
        let mut headers: Vec<(String, String)> = Vec::new();
        apply_static(&self.cfg, &mut headers, &mut url);
        if stream {
            url.query_pairs_mut().append_pair("alt", "sse");
        }
        let mut req = PreparedRequest::new(reqwest::Method::POST, url, format!("{}.chat", self.cfg.id));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req = crate::config::apply_auth(req, &self.cfg.auth, self.secret.as_deref())?;
        req = req.json_body(body)?;
        if stream {
            req = req.streaming().header("accept", "text/event-stream");
        }
        if let Some(ms) = self.cfg.timeout_ms {
            req = req.timeout(std::time::Duration::from_millis(ms));
        }
        Ok(req)
    }
}

fn parse_data_url(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("data:")?;
    let idx = rest.find(";base64,")?;
    Some((&rest[..idx], &rest[idx + 8..]))
}

/// Gemini rejects unsupported JSON-schema keywords such as `$schema`/`title`.
fn normalize_schema(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if k == "$schema" || k == "title" || k == "additionalProperties" {
                    continue;
                }
                out.insert(k.clone(), normalize_schema(v));
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(normalize_schema).collect()),
        other => other.clone(),
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn id(&self) -> &str {
        &self.cfg.id
    }
    fn info(&self) -> ProviderInfo {
        self.info.clone()
    }
    fn capabilities(&self) -> Capabilities {
        self.caps.clone()
    }
    fn default_model(&self) -> Option<String> {
        self.cfg.model.clone()
    }

    async fn list_models(&self, cancel: &CancellationToken) -> Result<Vec<ModelInfo>> {
        let mut url = join_url(&self.cfg.base_url, "models")?;
        let mut headers: Vec<(String, String)> = Vec::new();
        apply_static(&self.cfg, &mut headers, &mut url);
        let mut req = PreparedRequest::new(reqwest::Method::GET, url, format!("{}.models", self.cfg.id));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req = crate::config::apply_auth(req, &self.cfg.auth, self.secret.as_deref())?;
        let resp = self.http.execute(req, cancel).await?;
        let (status, headers, text) = read_body_limited(resp, 4 * 1024 * 1024).await;
        if !(200..300).contains(&status) {
            return Err(api_error(status, &headers, &text));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| EngineError::Decode(format!("invalid /models response: {e}")))?;
        let mut models = Vec::new();
        if let Some(arr) = v.get("models").and_then(Value::as_array) {
            for m in arr {
                let raw = m.get("name").and_then(Value::as_str).unwrap_or_default();
                let id = raw.strip_prefix("models/").unwrap_or(raw);
                if id.is_empty() {
                    continue;
                }
                models.push(ModelInfo {
                    id: id.to_string(),
                    display_name: m.get("displayName").and_then(Value::as_str).map(str::to_string),
                    context_window: m.get("inputTokenLimit").and_then(Value::as_u64).map(|v| v as u32),
                    max_output_tokens: m.get("outputTokenLimit").and_then(Value::as_u64).map(|v| v as u32),
                    owned_by: None,
                    created: None,
                });
            }
        }
        Ok(models)
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        let model = req.model.trim_start_matches("models/").to_string();
        let body = self.build_body(&req);
        let prepared = self.prepare(&model, &body, true)?;
        let resp = self.http.execute(prepared, &cancel).await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let (status, headers, text) = read_body_limited(resp, ERROR_BODY_LIMIT).await;
            return Err(api_error(status, &headers, &text));
        }
        let sse = crate::http::sse_stream(resp);
        Ok(map_gemini_stream(sse, self.cfg.id.clone(), model, cancel))
    }

    async fn validate(&self, cancel: &CancellationToken) -> Result<ValidationReport> {
        standard_validation(self, cancel).await
    }
}

struct GeminiState {
    provider: String,
    model: String,
    started: bool,
    done_sent: bool,
    finish: FinishReason,
    tool_index: usize,
    pending: VecDeque<Result<StreamEvent>>,
}

fn map_gemini_stream(
    sse: crate::http::SseStream,
    provider: String,
    model: String,
    cancel: CancellationToken,
) -> EventStream {
    use futures::StreamExt;
    let state = GeminiState {
        provider,
        model,
        started: false,
        done_sent: false,
        finish: FinishReason::Stop,
        tool_index: 0,
        pending: VecDeque::new(),
    };
    Box::pin(futures::stream::unfold(
        (sse, state, cancel),
        |(mut sse, mut st, cancel)| async move {
            loop {
                if let Some(item) = st.pending.pop_front() {
                    return Some((item, (sse, st, cancel)));
                }
                if cancel.is_cancelled() {
                    return None;
                }
                match sse.next().await {
                    None => {
                        if st.started && !st.done_sent {
                            st.done_sent = true;
                            return Some((
                                Ok(StreamEvent::Done {
                                    finish_reason: st.finish.clone(),
                                }),
                                (sse, st, cancel),
                            ));
                        }
                        return None;
                    }
                    Some(Err(e)) => return Some((Err(e), (sse, st, cancel))),
                    Some(Ok(ev)) => {
                        if !st.started {
                            st.started = true;
                            st.pending.push_back(Ok(StreamEvent::Started {
                                provider: st.provider.clone(),
                                model: st.model.clone(),
                            }));
                        }
                        if ev.is_done() || ev.data.trim().is_empty() {
                            continue;
                        }
                        let v: Value = match serde_json::from_str(&ev.data) {
                            Ok(v) => v,
                            Err(e) => {
                                st.pending.push_back(Err(EngineError::Decode(format!(
                                    "malformed SSE chunk: {e}"
                                ))));
                                continue;
                            }
                        };
                        if let Some(err) = v.get("error") {
                            let msg = err.get("message").and_then(Value::as_str).unwrap_or("gemini error");
                            st.pending.push_back(Err(EngineError::Other(msg.to_string())));
                            continue;
                        }
                        if let Some(meta) = v.get("usageMetadata") {
                            let prompt = meta.get("promptTokenCount").and_then(Value::as_u64).unwrap_or(0) as u32;
                            let completion = meta.get("candidatesTokenCount").and_then(Value::as_u64).unwrap_or(0) as u32;
                            let total = meta.get("totalTokenCount").and_then(Value::as_u64).unwrap_or((prompt + completion) as u64) as u32;
                            st.pending.push_back(Ok(StreamEvent::Usage(crate::response::Usage {
                                prompt_tokens: prompt,
                                completion_tokens: completion,
                                total_tokens: total,
                                ..Default::default()
                            })));
                        }
                        if let Some(cand) = v.get("candidates").and_then(|c| c.get(0)) {
                            if let Some(parts) = cand
                                .get("content")
                                .and_then(|c| c.get("parts"))
                                .and_then(Value::as_array)
                            {
                                for p in parts {
                                    if let Some(t) = p.get("text").and_then(Value::as_str) {
                                        if !t.is_empty() {
                                            st.pending.push_back(Ok(StreamEvent::ContentDelta(t.to_string())));
                                        }
                                    }
                                    if let Some(fc) = p.get("functionCall") {
                                        let name = fc.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                                        let args = fc
                                            .get("args")
                                            .map(|a| a.to_string())
                                            .unwrap_or_else(|| "{}".into());
                                        let idx = st.tool_index;
                                        st.tool_index += 1;
                                        st.pending.push_back(Ok(StreamEvent::ToolCallDelta {
                                            index: idx,
                                            id: Some(format!("call_{idx}")),
                                            name: Some(name),
                                            arguments_delta: args,
                                        }));
                                    }
                                }
                            }
                            if let Some(fr) = cand.get("finishReason").and_then(Value::as_str) {
                                st.finish = FinishReason::parse(fr);
                                if !st.done_sent {
                                    st.done_sent = true;
                                    st.pending.push_back(Ok(StreamEvent::Done {
                                        finish_reason: st.finish.clone(),
                                    }));
                                }
                            }
                        }
                    }
                }
            }
        },
    ))
}
