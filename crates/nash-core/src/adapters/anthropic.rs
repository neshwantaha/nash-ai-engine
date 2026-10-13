//! Adapter for the Anthropic Messages API (`POST /v1/messages`).
//!
//! Anthropic differs from OpenAI in several ways this adapter handles:
//! * the system prompt is a top-level field, not a message;
//! * `max_tokens` is required;
//! * tool results are `user` turns containing `tool_result` blocks;
//! * streaming uses named SSE events (`content_block_delta`, ...).

use super::common::*;
use crate::capability::{Capabilities, Support};
use crate::config::ProviderConfig;
use crate::error::{EngineError, Result};
use crate::http::{HttpClient, PreparedRequest, SseEvent};
use crate::message::{ChatMessage, ContentPart, Role, ToolCall, ToolChoice};
use crate::provider::{ModelInfo, Provider, ProviderInfo, ValidationReport};
use crate::request::{ChatRequest, ResponseFormat};
use crate::response::{FinishReason, Usage};
use crate::stream::{EventStream, StreamEvent};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;

const DEFAULT_MAX_TOKENS: u32 = 4096;
const DEFAULT_ANTHROPIC_VERSION: &str = "2023-06-01";

#[derive(Debug)]
pub struct AnthropicProvider {
    cfg: ProviderConfig,
    secret: Option<String>,
    http: HttpClient,
    info: ProviderInfo,
    caps: Capabilities,
}

impl AnthropicProvider {
    pub fn new(cfg: ProviderConfig, secret: Option<String>, http: HttpClient) -> Result<Self> {
        let info = provider_info(
            &cfg,
            "Anthropic",
            Some("https://docs.anthropic.com/en/api/messages"),
        );
        let caps = Capabilities {
            streaming: Support::Supported,
            tools: Support::Supported,
            parallel_tool_calls: Support::Supported,
            vision: Support::Supported,
            reasoning: Support::Supported,
            usage_reporting: Support::Supported,
            model_listing: Support::Supported,
            context_window: Some(200_000),
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

    fn build_body(&self, req: &ChatRequest, stream: bool) -> Value {
        let (system, messages) = anthropic_messages(&req.messages);
        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(req.model));
        body.insert(
            "max_tokens".into(),
            json!(req.params.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS)),
        );
        if !system.is_empty() {
            body.insert("system".into(), json!(system));
        }
        body.insert("messages".into(), Value::Array(messages));
        body.insert("stream".into(), json!(stream));
        let p = &req.params;
        if let Some(t) = p.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(tp) = p.top_p {
            body.insert("top_p".into(), json!(tp));
        }
        if !p.stop.is_empty() {
            body.insert("stop_sequences".into(), json!(p.stop));
        }
        if let Some(effort) = &p.reasoning_effort {
            // Map a coarse effort label onto Anthropic's thinking budget.
            let budget = match effort.as_str() {
                "low" => 1024,
                "medium" => 4096,
                "high" => 16384,
                _ => effort.parse::<u32>().unwrap_or(4096),
            };
            body.insert("thinking".into(), json!({"type": "enabled", "budget_tokens": budget}));
        }
        if req.wants_tools() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));
            body.insert(
                "tool_choice".into(),
                match &req.tool_choice {
                    ToolChoice::Auto => json!({"type": "auto"}),
                    ToolChoice::None => json!({"type": "auto"}),
                    ToolChoice::Required => json!({"type": "any"}),
                    ToolChoice::Specific(name) => json!({"type": "tool", "name": name}),
                },
            );
        }
        if matches!(p.response_format, ResponseFormat::JsonObject) {
            body.insert("metadata".into(), json!({"nash_response_format": "json"}));
        }
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }
        Value::Object(body)
    }

    fn prepare(&self, path: &str, body: &Value, stream: bool) -> Result<PreparedRequest> {
        let mut url = join_url(&self.cfg.base_url, path)?;
        let mut headers: Vec<(String, String)> = Vec::new();
        apply_static(&self.cfg, &mut headers, &mut url);
        let has_version = headers
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("anthropic-version"));
        let mut req = PreparedRequest::new(reqwest::Method::POST, url, format!("{}.chat", self.cfg.id));
        if !has_version {
            req = req.header("anthropic-version", DEFAULT_ANTHROPIC_VERSION);
        }
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

/// Convert engine messages into (system_prompt, anthropic_messages), merging
/// consecutive same-role turns so the strict alternation rule is respected.
fn anthropic_messages(messages: &[ChatMessage]) -> (String, Vec<Value>) {
    let mut system = String::new();
    let mut turns: Vec<(String, Vec<Value>)> = Vec::new();

    let mut push = |role: &str, blocks: Vec<Value>| {
        if blocks.is_empty() {
            return;
        }
        if let Some(last) = turns.last_mut() {
            if last.0 == role {
                last.1.extend(blocks);
                return;
            }
        }
        turns.push((role.to_string(), blocks));
    };

    for m in messages {
        match m.role {
            Role::System => {
                if !system.is_empty() {
                    system.push('\n');
                }
                system.push_str(&m.text());
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                let text = m.text();
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for tc in &m.tool_calls {
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": tc.id,
                        "name": tc.name,
                        "input": tc.arguments
                    }));
                }
                push("assistant", blocks);
            }
            Role::User => {
                let mut blocks = Vec::new();
                for p in &m.content {
                    match p {
                        ContentPart::Text { text } => {
                            blocks.push(json!({"type": "text", "text": text}));
                        }
                        ContentPart::ImageUrl { url, .. } => {
                            blocks.push(json!({
                                "type": "image",
                                "source": {"type": "url", "url": url}
                            }));
                        }
                    }
                }
                push("user", blocks);
            }
            Role::Tool => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": m.tool_call_id.clone().unwrap_or_default(),
                    "content": m.text()
                });
                push("user", vec![block]);
            }
        }
    }

    let msg_values = turns
        .into_iter()
        .map(|(role, blocks)| {
            // A single text block can be collapsed to a string.
            let content = if blocks.len() == 1 && blocks[0]["type"] == "text" {
                blocks[0]["text"].clone()
            } else {
                Value::Array(blocks)
            };
            json!({"role": role, "content": content})
        })
        .collect();
    (system, msg_values)
}

#[async_trait]
impl Provider for AnthropicProvider {
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
        req = req.header("anthropic-version", DEFAULT_ANTHROPIC_VERSION);
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req = crate::config::apply_auth(req, &self.cfg.auth, self.secret.as_deref())?;
        let resp = self.http.execute(req, cancel).await?;
        let (status, headers, text) = read_body_limited(resp, 4 * 1024 * 1024).await;
        if !(200..300).contains(&status) {
            if status == 404 {
                return Err(EngineError::Unsupported(
                    "Anthropic/this endpoint has no /models listing".into(),
                ));
            }
            return Err(api_error(status, &headers, &text));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| EngineError::Decode(format!("invalid /models response: {e}")))?;
        let mut models = Vec::new();
        if let Some(arr) = v.get("data").and_then(Value::as_array) {
            for m in arr {
                if let Some(id) = m.get("id").and_then(Value::as_str) {
                    models.push(ModelInfo {
                        id: id.to_string(),
                        display_name: m.get("display_name").and_then(Value::as_str).map(str::to_string),
                        ..ModelInfo::new(id)
                    });
                }
            }
        }
        Ok(models)
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        if req.is_multimodal() && self.caps.vision.is_known_unsupported() {
            return Err(EngineError::Unsupported("vision not supported".into()));
        }
        if req.wants_tools() && self.caps.tools.is_known_unsupported() {
            return Err(EngineError::Unsupported("tool calling not supported".into()));
        }
        let body = self.build_body(&req, true);
        let prepared = self.prepare("messages", &body, true)?;
        let resp = self.http.execute(prepared, &cancel).await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let (status, headers, text) = read_body_limited(resp, ERROR_BODY_LIMIT).await;
            return Err(api_error(status, &headers, &text));
        }
        let sse = crate::http::sse_stream(resp);
        Ok(map_anthropic_stream(sse, self.cfg.id.clone(), req.model, cancel))
    }

    async fn validate(&self, cancel: &CancellationToken) -> Result<ValidationReport> {
        standard_validation(self, cancel).await
    }
}

struct AnthropicState {
    provider: String,
    model: String,
    started: bool,
    done_sent: bool,
    finish: FinishReason,
    usage: Usage,
    pending: VecDeque<Result<StreamEvent>>,
}

fn map_anthropic_stream(
    sse: crate::http::SseStream,
    provider: String,
    model: String,
    cancel: CancellationToken,
) -> EventStream {
    use futures::StreamExt;
    let state = AnthropicState {
        provider,
        model,
        started: false,
        done_sent: false,
        finish: FinishReason::Stop,
        usage: Usage::default(),
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
                        if !st.done_sent {
                            st.done_sent = true;
                            let usage = st.usage.clone().normalised();
                            st.pending.push_back(Ok(StreamEvent::Usage(usage)));
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
                        handle_anthropic_event(&mut st, &ev);
                    }
                }
            }
        },
    ))
}

fn handle_anthropic_event(st: &mut AnthropicState, ev: &SseEvent) {
    if !st.started {
        st.started = true;
        st.pending.push_back(Ok(StreamEvent::Started {
            provider: st.provider.clone(),
            model: st.model.clone(),
        }));
    }
    if ev.data.trim().is_empty() {
        return;
    }
    let v: Value = match serde_json::from_str(&ev.data) {
        Ok(v) => v,
        Err(e) => {
            st.pending
                .push_back(Err(EngineError::Decode(format!("malformed SSE chunk: {e}"))));
            return;
        }
    };
    let kind = ev
        .event
        .clone()
        .or_else(|| v.get("type").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    match kind.as_str() {
        "message_start" => {
            if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                if let Some(p) = u.get("input_tokens").and_then(Value::as_u64) {
                    st.usage.prompt_tokens = p as u32;
                }
            }
        }
        "content_block_start" => {
            let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            if let Some(cb) = v.get("content_block") {
                if cb.get("type").and_then(Value::as_str) == Some("tool_use") {
                    st.pending.push_back(Ok(StreamEvent::ToolCallDelta {
                        index,
                        id: cb.get("id").and_then(Value::as_str).map(str::to_string),
                        name: cb.get("name").and_then(Value::as_str).map(str::to_string),
                        arguments_delta: String::new(),
                    }));
                }
            }
        }
        "content_block_delta" => {
            let index = v.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            if let Some(delta) = v.get("delta") {
                match delta.get("type").and_then(Value::as_str).unwrap_or("") {
                    "text_delta" => {
                        if let Some(t) = delta.get("text").and_then(Value::as_str) {
                            st.pending
                                .push_back(Ok(StreamEvent::ContentDelta(t.to_string())));
                        }
                    }
                    "thinking_delta" => {
                        if let Some(t) = delta.get("thinking").and_then(Value::as_str) {
                            st.pending
                                .push_back(Ok(StreamEvent::ReasoningDelta(t.to_string())));
                        }
                    }
                    "input_json_delta" => {
                        if let Some(t) = delta.get("partial_json").and_then(Value::as_str) {
                            st.pending.push_back(Ok(StreamEvent::ToolCallDelta {
                                index,
                                id: None,
                                name: None,
                                arguments_delta: t.to_string(),
                            }));
                        }
                    }
                    _ => {}
                }
            }
        }
        "message_delta" => {
            if let Some(delta) = v.get("delta") {
                if let Some(fr) = delta.get("stop_reason").and_then(Value::as_str) {
                    st.finish = FinishReason::parse(fr);
                }
            }
            if let Some(u) = v.get("usage") {
                if let Some(o) = u.get("output_tokens").and_then(Value::as_u64) {
                    st.usage.completion_tokens = o as u32;
                }
            }
        }
        "message_stop" => {
            if !st.done_sent {
                st.done_sent = true;
                st.pending.push_back(Ok(StreamEvent::Usage(st.usage.clone().normalised())));
                st.pending.push_back(Ok(StreamEvent::Done {
                    finish_reason: st.finish.clone(),
                }));
            }
        }
        "error" => {
            let msg = v
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("anthropic stream error");
            st.pending.push_back(Err(EngineError::Other(msg.to_string())));
        }
        "ping" => {}
        _ => {}
    }
}

#[allow(dead_code)]
fn _tool_call_helper(tc: &ToolCall) -> Value {
    json!({"type": "tool_use", "id": tc.id, "name": tc.name, "input": tc.arguments})
}
