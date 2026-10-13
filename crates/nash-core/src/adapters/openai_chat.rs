//! Adapter for the OpenAI **Chat Completions** protocol.
//!
//! This single adapter powers OpenAI, Azure OpenAI, OpenRouter, DeepSeek,
//! Groq, Mistral, xAI, Together, Fireworks, vLLM, LM Studio and any other
//! service that implements `POST {base}/chat/completions`. Provider-specific
//! differences are expressed through `base_url`, auth, headers, query params
//! and the `extra` request fields — not through code.

use super::common::*;
use crate::capability::Capabilities;
use crate::config::ProviderConfig;
use crate::error::{EngineError, Result};
use crate::http::{HttpClient, PreparedRequest, SseEvent};
use crate::provider::{CheckResult, ModelInfo, Provider, ProviderInfo, ValidationReport};
use crate::request::{ChatRequest, ResponseFormat};
use crate::stream::{EventStream, StreamEvent};
use crate::response::FinishReason;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

/// OpenAI Chat Completions adapter.
#[derive(Debug)]
pub struct OpenAiChatProvider {
    cfg: ProviderConfig,
    secret: Option<String>,
    http: HttpClient,
    info: ProviderInfo,
    caps: Capabilities,
}

impl OpenAiChatProvider {
    pub fn new(cfg: ProviderConfig, secret: Option<String>, http: HttpClient) -> Result<Self> {
        let info = provider_info(&cfg, &cfg.id, Some("https://platform.openai.com/docs/api-reference/chat"));
        let caps = Capabilities::openai_chat_baseline().overlay(&cfg.capabilities);
        Ok(Self {
            cfg,
            secret,
            http,
            info,
            caps,
        })
    }

    fn endpoint(&self, path: &str) -> Result<url::Url> {
        join_url(&self.cfg.base_url, path)
    }

    fn build_body(&self, req: &ChatRequest, stream: bool) -> Value {
        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(req.model));
        body.insert(
            "messages".into(),
            Value::Array(req.messages.iter().map(openai_message).collect()),
        );
        if stream {
            body.insert("stream".into(), json!(true));
            if !self.caps.usage_reporting.is_known_unsupported() {
                body.insert("stream_options".into(), json!({"include_usage": true}));
            }
        }
        let p = &req.params;
        if let Some(t) = p.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(tp) = p.top_p {
            body.insert("top_p".into(), json!(tp));
        }
        if let Some(m) = p.max_tokens {
            body.insert("max_tokens".into(), json!(m));
        }
        if !p.stop.is_empty() {
            body.insert("stop".into(), json!(p.stop));
        }
        if let Some(s) = p.seed {
            body.insert("seed".into(), json!(s));
        }
        if let Some(pp) = p.presence_penalty {
            body.insert("presence_penalty".into(), json!(pp));
        }
        if let Some(fp) = p.frequency_penalty {
            body.insert("frequency_penalty".into(), json!(fp));
        }
        if let Some(re) = &p.reasoning_effort {
            body.insert("reasoning_effort".into(), json!(re));
        }
        match &p.response_format {
            ResponseFormat::Text => {}
            ResponseFormat::JsonObject => {
                body.insert("response_format".into(), json!({"type": "json_object"}));
            }
            ResponseFormat::JsonSchema { name, schema, strict } => {
                body.insert(
                    "response_format".into(),
                    json!({
                        "type": "json_schema",
                        "json_schema": {"name": name, "schema": schema, "strict": strict}
                    }),
                );
            }
        }
        if req.wants_tools() {
            body.insert("tools".into(), openai_tools(&req.tools));
            body.insert("tool_choice".into(), openai_tool_choice(&req.tool_choice));
        }
        // Provider-specific passthrough.
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }
        Value::Object(body)
    }

    fn prepare(&self, path: &str, body: &Value, stream: bool) -> Result<PreparedRequest> {
        let mut url = self.endpoint(path)?;
        let mut headers: Vec<(String, String)> = Vec::new();
        apply_static(&self.cfg, &mut headers, &mut url);
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

#[async_trait]
impl Provider for OpenAiChatProvider {
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
        let mut url = self.endpoint("models")?;
        let mut headers: Vec<(String, String)> = Vec::new();
        apply_static(&self.cfg, &mut headers, &mut url);
        let mut req = PreparedRequest::new(reqwest::Method::GET, url, format!("{}.models", self.cfg.id));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req = crate::config::apply_auth(req, &self.cfg.auth, self.secret.as_deref())?;

        let resp = match self.http.execute(req, cancel).await {
            Ok(r) => r,
            Err(e) => return Err(e),
        };
        let (status, headers, text) = read_body_limited(resp, 4 * 1024 * 1024).await;
        if !(200..300).contains(&status) {
            if status == 404 {
                return Err(EngineError::Unsupported(
                    "this endpoint does not expose a /models listing".into(),
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
                        display_name: None,
                        context_window: m.get("context_length").and_then(Value::as_u64).map(|v| v as u32),
                        max_output_tokens: None,
                        owned_by: m.get("owned_by").and_then(Value::as_str).map(str::to_string),
                        created: m.get("created").and_then(Value::as_i64),
                    });
                }
            }
        } else if let Some(arr) = v.get("models").and_then(Value::as_array) {
            // Some compatible servers use a bare array under "models".
            for m in arr {
                let id = m
                    .get("id")
                    .or_else(|| m.get("name"))
                    .and_then(Value::as_str);
                if let Some(id) = id {
                    models.push(ModelInfo::new(id));
                }
            }
        }
        Ok(models)
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        if req.is_multimodal() && self.caps.vision.is_known_unsupported() {
            return Err(EngineError::Unsupported(format!(
                "provider '{}' does not support image inputs",
                self.cfg.id
            )));
        }
        if req.wants_tools() && self.caps.tools.is_known_unsupported() {
            return Err(EngineError::Unsupported(format!(
                "provider '{}' does not support tool calling",
                self.cfg.id
            )));
        }
        let body = self.build_body(&req, true);
        let prepared = self.prepare("chat/completions", &body, true)?;
        let resp = self.http.execute(prepared, &cancel).await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let (status, headers, text) = read_body_limited(resp, ERROR_BODY_LIMIT).await;
            return Err(api_error(status, &headers, &text));
        }
        let sse = crate::http::sse_stream(resp);
        Ok(map_openai_stream(sse, self.cfg.id.clone(), req.model, cancel))
    }

    async fn validate(&self, cancel: &CancellationToken) -> Result<ValidationReport> {
        let mut report = ValidationReport::new(&self.cfg.id);

        // 1. Authentication / connectivity via model listing.
        let started = Instant::now();
        match self.list_models(cancel).await {
            Ok(models) => {
                let ms = started.elapsed().as_millis() as u64;
                let detail = if models.is_empty() {
                    "authenticated; no models advertised".to_string()
                } else {
                    format!("authenticated; {} model(s) available", models.len())
                };
                report.push(CheckResult::pass("authentication", Some(detail)).with_latency(ms));
                // 2. Model availability.
                let want = self.cfg.model.clone();
                match want {
                    Some(m) if !models.iter().any(|x| x.id == m) => {
                        report.push(CheckResult::warn(
                            "model_availability",
                            format!("configured model '{m}' was not in the /models list"),
                        ));
                    }
                    Some(m) => {
                        report.push(CheckResult::pass(
                            "model_availability",
                            Some(format!("model '{m}' present")),
                        ));
                    }
                    None => {
                        report.push(CheckResult::skip(
                            "model_availability",
                            "no model configured; set one to check availability",
                        ));
                    }
                }
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
                    "endpoint has no /models listing; login cannot be verified this way",
                ));
                report.push(CheckResult::skip(
                    "model_availability",
                    "no /models endpoint",
                ));
            }
            Err(e) => {
                report.push(CheckResult::fail("authentication", e.to_string()));
                report.push(CheckResult::skip("model_availability", "connection failed"));
                report.push(CheckResult::skip("streaming_generation", "connection failed"));
                return Ok(report);
            }
        }

        // 3. Basic generation + streaming (one cheap streaming request).
        let model = self.cfg.model.clone().unwrap_or_else(|| "gpt-4o-mini".to_string());
        let probe = ChatRequest::new(model).message(crate::message::ChatMessage::user("ping")).max_tokens(1);
        let started = Instant::now();
        match self.stream(probe, cancel.clone()).await {
            Ok(mut s) => {
                use futures::StreamExt;
                let mut got_event = false;
                while let Some(ev) = s.next().await {
                    match ev {
                        Ok(_) => got_event = true,
                        Err(e) => {
                            report.push(CheckResult::fail("streaming_generation", e.to_string()));
                            return Ok(report);
                        }
                    }
                    if got_event {
                        // First event is enough to prove streaming works and
                        // keeps the probe as cheap as possible.
                        break;
                    }
                }
                let ms = started.elapsed().as_millis() as u64;
                if got_event {
                    report.push(
                        CheckResult::pass(
                            "streaming_generation",
                            Some("received first streamed event".into()),
                        )
                        .with_latency(ms),
                    );
                } else {
                    report.push(CheckResult::fail(
                        "streaming_generation",
                        "stream produced no events",
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
        Ok(report)
    }
}

struct OpenAiStreamState {
    provider: String,
    model: String,
    started: bool,
    done_sent: bool,
    finish: FinishReason,
    pending: VecDeque<Result<StreamEvent>>,
}

impl OpenAiStreamState {
    fn new(provider: String, model: String) -> Self {
        Self {
            provider,
            model,
            started: false,
            done_sent: false,
            finish: FinishReason::Stop,
            pending: VecDeque::new(),
        }
    }

    fn handle(&mut self, ev: &SseEvent) {
        if !self.started {
            self.pending.push_back(Ok(StreamEvent::Started {
                provider: self.provider.clone(),
                model: self.model.clone(),
            }));
            self.started = true;
        }
        if ev.is_done() {
            if !self.done_sent {
                self.done_sent = true;
                self.pending.push_back(Ok(StreamEvent::Done {
                    finish_reason: self.finish.clone(),
                }));
            }
            return;
        }
        if ev.data.trim().is_empty() {
            return;
        }
        let v: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(e) => {
                self.pending.push_back(Err(EngineError::Decode(format!(
                    "malformed stream chunk: {e}"
                ))));
                return;
            }
        };
        if let Some(err) = v.get("error") {
            let msg = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("stream error");
            self.pending.push_back(Err(EngineError::Other(msg.to_string())));
            return;
        }
        if let Some(usage) = v.get("usage").filter(|u| !u.is_null()) {
            if let Some(u) = parse_openai_usage(usage) {
                self.pending.push_back(Ok(StreamEvent::Usage(u)));
            }
        }
        let choice = v.get("choices").and_then(|c| c.get(0));
        let Some(choice) = choice else {
            return;
        };
        if let Some(delta) = choice.get("delta") {
            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                if !content.is_empty() {
                    self.pending
                        .push_back(Ok(StreamEvent::ContentDelta(content.to_string())));
                }
            }
            for key in ["reasoning_content", "reasoning"] {
                if let Some(r) = delta.get(key).and_then(Value::as_str) {
                    if !r.is_empty() {
                        self.pending
                            .push_back(Ok(StreamEvent::ReasoningDelta(r.to_string())));
                    }
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
                for (i, tc) in calls.iter().enumerate() {
                    let index = tc.get("index").and_then(Value::as_u64).map(|v| v as usize).unwrap_or(i);
                    let id = tc.get("id").and_then(Value::as_str).map(str::to_string);
                    let func = tc.get("function");
                    let name = func
                        .and_then(|f| f.get("name"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let args = func
                        .and_then(|f| f.get("arguments"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    self.pending.push_back(Ok(StreamEvent::ToolCallDelta {
                        index,
                        id,
                        name,
                        arguments_delta: args,
                    }));
                }
            }
        }
        if let Some(fr) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = FinishReason::parse(fr);
            if !self.done_sent {
                self.done_sent = true;
                self.pending.push_back(Ok(StreamEvent::Done {
                    finish_reason: self.finish.clone(),
                }));
            }
        }
    }
}

fn map_openai_stream(
    sse: crate::http::SseStream,
    provider: String,
    model: String,
    cancel: CancellationToken,
) -> EventStream {
    use futures::StreamExt;
    let state = OpenAiStreamState::new(provider, model);
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
                        st.handle(&ev);
                    }
                }
            }
        },
    ))
}

/// Convenience for tests: the raw SSE→event mapping without a network client.
#[doc(hidden)]
pub fn parse_openai_chunk_for_test(data: &str) -> Vec<StreamEvent> {
    let mut st = OpenAiStreamState::new("test".into(), "test".into());
    st.handle(&SseEvent {
        event: None,
        data: data.to_string(),
        id: None,
        retry: None,
    });
    st.pending.into_iter().filter_map(|r| r.ok()).collect()
}
