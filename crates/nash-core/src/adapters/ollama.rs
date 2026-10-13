//! Adapter for the Ollama native API (`POST /api/chat`, NDJSON streaming).
//!
//! Ollama is a local runtime, so it is marked `local` and exempt from the
//! "remote endpoints need a credential" rule. Its streaming wire format is
//! newline-delimited JSON, not SSE.

use super::common::*;
use crate::capability::{Capabilities, Support};
use crate::config::{AuthConfig, ProviderConfig};
use crate::error::{EngineError, Result};
use crate::http::{HttpClient, PreparedRequest};
use crate::message::{ChatMessage, ContentPart, Role};
use crate::provider::{ModelInfo, Provider, ProviderInfo, ValidationReport};
use crate::request::ChatRequest;
use crate::response::FinishReason;
use crate::stream::{EventStream, StreamEvent};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct OllamaProvider {
    cfg: ProviderConfig,
    secret: Option<String>,
    http: HttpClient,
    info: ProviderInfo,
    caps: Capabilities,
}

impl OllamaProvider {
    pub fn new(mut cfg: ProviderConfig, secret: Option<String>, http: HttpClient) -> Result<Self> {
        if cfg.base_url.trim().is_empty() {
            cfg.base_url = "http://localhost:11434".to_string();
        }
        let info = provider_info(&cfg, "Ollama", Some("https://github.com/ollama/ollama/blob/main/docs/api.md"));
        let caps = Capabilities {
            streaming: Support::Supported,
            tools: Support::Supported,
            json_mode: Support::Supported,
            structured_output: Support::Supported,
            vision: Support::Supported,
            usage_reporting: Support::Supported,
            model_listing: Support::Supported,
            embeddings: Support::Supported,
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

    fn build_body(&self, req: &ChatRequest, stream: bool) -> Result<Value> {
        let mut messages = Vec::new();
        for m in &req.messages {
            messages.push(ollama_message(m)?);
        }
        let mut options = serde_json::Map::new();
        let p = &req.params;
        if let Some(t) = p.temperature {
            options.insert("temperature".into(), json!(t));
        }
        if let Some(tp) = p.top_p {
            options.insert("top_p".into(), json!(tp));
        }
        if let Some(m) = p.max_tokens {
            options.insert("num_predict".into(), json!(m));
        }
        if !p.stop.is_empty() {
            options.insert("stop".into(), json!(p.stop));
        }
        if let Some(s) = p.seed {
            options.insert("seed".into(), json!(s));
        }

        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(req.model));
        body.insert("messages".into(), Value::Array(messages));
        body.insert("stream".into(), json!(stream));
        if !options.is_empty() {
            body.insert("options".into(), Value::Object(options));
        }
        if req.wants_tools() {
            body.insert("tools".into(), openai_tools(&req.tools));
        }
        if matches!(p.response_format, crate::request::ResponseFormat::JsonObject) {
            body.insert("format".into(), json!("json"));
        }
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }
        Ok(Value::Object(body))
    }

    fn prepare(&self, body: &Value, stream: bool) -> Result<PreparedRequest> {
        let mut url = join_url(&self.cfg.base_url, "api/chat")?;
        let mut headers: Vec<(String, String)> = Vec::new();
        apply_static(&self.cfg, &mut headers, &mut url);
        let mut req = PreparedRequest::new(reqwest::Method::POST, url, format!("{}.chat", self.cfg.id));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        // Local servers normally need no auth; only apply when a secret exists.
        if self.cfg.auth != AuthConfig::None && self.secret.is_some() {
            req = crate::config::apply_auth(req, &self.cfg.auth, self.secret.as_deref())?;
        }
        req = req.json_body(body)?;
        if stream {
            req = req.streaming();
        }
        if let Some(ms) = self.cfg.timeout_ms {
            req = req.timeout(std::time::Duration::from_millis(ms));
        }
        Ok(req)
    }
}

fn ollama_message(m: &ChatMessage) -> Result<Value> {
    let mut obj = serde_json::Map::new();
    obj.insert("role".into(), json!(m.role.as_str()));
    obj.insert("content".into(), json!(m.text()));
    // Ollama accepts base64 images under `images`.
    let mut images = Vec::new();
    for part in &m.content {
        if let ContentPart::ImageUrl { url, .. } = part {
            if let Some(b64) = url.strip_prefix("data:") {
                if let Some(idx) = b64.find(";base64,") {
                    images.push(json!(&b64[idx + 8..]));
                    continue;
                }
            }
            return Err(EngineError::Unsupported(
                "Ollama requires base64 `data:` image URLs; remote image URLs are not supported".into(),
            ));
        }
    }
    if !images.is_empty() {
        obj.insert("images".into(), Value::Array(images));
    }
    if m.role == Role::Tool {
        if let Some(id) = &m.tool_call_id {
            obj.insert("tool_call_id".into(), json!(id));
        }
    }
    Ok(Value::Object(obj))
}

#[async_trait]
impl Provider for OllamaProvider {
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
        let mut url = join_url(&self.cfg.base_url, "api/tags")?;
        let mut headers: Vec<(String, String)> = Vec::new();
        apply_static(&self.cfg, &mut headers, &mut url);
        let mut req = PreparedRequest::new(reqwest::Method::GET, url, format!("{}.tags", self.cfg.id));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        let resp = self.http.execute(req, cancel).await?;
        let (status, headers, text) = read_body_limited(resp, 4 * 1024 * 1024).await;
        if !(200..300).contains(&status) {
            return Err(api_error(status, &headers, &text));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| EngineError::Decode(format!("invalid /api/tags response: {e}")))?;
        let mut models = Vec::new();
        if let Some(arr) = v.get("models").and_then(Value::as_array) {
            for m in arr {
                let id = m
                    .get("model")
                    .or_else(|| m.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if id.is_empty() {
                    continue;
                }
                models.push(ModelInfo::new(id));
            }
        }
        Ok(models)
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        let body = self.build_body(&req, true)?;
        let prepared = self.prepare(&body, true)?;
        let resp = self.http.execute(prepared, &cancel).await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let (status, headers, text) = read_body_limited(resp, ERROR_BODY_LIMIT).await;
            return Err(api_error(status, &headers, &text));
        }
        let lines = crate::http::ndjson_stream(resp);
        Ok(map_ollama_stream(lines, self.cfg.id.clone(), req.model, cancel))
    }

    async fn validate(&self, cancel: &CancellationToken) -> Result<ValidationReport> {
        standard_validation(self, cancel).await
    }
}

struct OllamaState {
    provider: String,
    model: String,
    started: bool,
    done_sent: bool,
    tools_sent: usize,
    pending: VecDeque<Result<StreamEvent>>,
}

fn map_ollama_stream(
    lines: crate::http::NdjsonStream,
    provider: String,
    model: String,
    cancel: CancellationToken,
) -> EventStream {
    use futures::StreamExt;
    let state = OllamaState {
        provider,
        model,
        started: false,
        done_sent: false,
        tools_sent: 0,
        pending: VecDeque::new(),
    };
    Box::pin(futures::stream::unfold(
        (lines, state, cancel),
        |(mut lines, mut st, cancel)| async move {
            loop {
                if let Some(item) = st.pending.pop_front() {
                    return Some((item, (lines, st, cancel)));
                }
                if cancel.is_cancelled() {
                    return None;
                }
                match lines.next().await {
                    None => {
                        if !st.done_sent {
                            st.done_sent = true;
                            return Some((
                                Ok(StreamEvent::Done {
                                    finish_reason: FinishReason::Stop,
                                }),
                                (lines, st, cancel),
                            ));
                        }
                        return None;
                    }
                    Some(Err(e)) => return Some((Err(e), (lines, st, cancel))),
                    Some(Ok(line)) => {
                        if !st.started {
                            st.started = true;
                            st.pending.push_back(Ok(StreamEvent::Started {
                                provider: st.provider.clone(),
                                model: st.model.clone(),
                            }));
                        }
                        let v: Value = match serde_json::from_str(&line) {
                            Ok(v) => v,
                            Err(e) => {
                                st.pending.push_back(Err(EngineError::Decode(format!(
                                    "malformed NDJSON line: {e}"
                                ))));
                                continue;
                            }
                        };
                        if let Some(err) = v.get("error").and_then(Value::as_str) {
                            st.pending.push_back(Err(EngineError::Other(err.to_string())));
                            continue;
                        }
                        if let Some(msg) = v.get("message") {
                            if let Some(content) = msg.get("content").and_then(Value::as_str) {
                                if !content.is_empty() {
                                    st.pending.push_back(Ok(StreamEvent::ContentDelta(
                                        content.to_string(),
                                    )));
                                }
                            }
                            if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
                                for tc in calls {
                                    let func = tc.get("function").cloned().unwrap_or(Value::Null);
                                    let name = func
                                        .get("name")
                                        .and_then(Value::as_str)
                                        .unwrap_or("")
                                        .to_string();
                                    let args = func
                                        .get("arguments")
                                        .map(|a| a.to_string())
                                        .unwrap_or_else(|| "{}".into());
                                    let idx = st.tools_sent;
                                    st.tools_sent += 1;
                                    st.pending.push_back(Ok(StreamEvent::ToolCallDelta {
                                        index: idx,
                                        id: Some(format!("call_{idx}")),
                                        name: Some(name),
                                        arguments_delta: args,
                                    }));
                                }
                            }
                        }
                        let done = v.get("done").and_then(Value::as_bool).unwrap_or(false);
                        if done {
                            let fr = v
                                .get("done_reason")
                                .and_then(Value::as_str)
                                .map(FinishReason::parse)
                                .unwrap_or(FinishReason::Stop);
                            let usage = ollama_usage(&v);
                            if let Some(u) = usage {
                                st.pending.push_back(Ok(StreamEvent::Usage(u)));
                            }
                            if !st.done_sent {
                                st.done_sent = true;
                                st.pending.push_back(Ok(StreamEvent::Done { finish_reason: fr }));
                            }
                        }
                    }
                }
            }
        },
    ))
}

fn ollama_usage(v: &Value) -> Option<crate::response::Usage> {
    let prompt = v.get("prompt_eval_count").and_then(Value::as_u64)? as u32;
    let completion = v.get("eval_count").and_then(Value::as_u64).unwrap_or(0) as u32;
    Some(crate::response::Usage {
        prompt_tokens: prompt,
        completion_tokens: completion,
        total_tokens: prompt + completion,
        ..Default::default()
    })
}
