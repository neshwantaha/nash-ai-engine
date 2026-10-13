//! Adapter for the OpenAI **Responses** API (`POST /responses`).
//!
//! Distinct from Chat Completions: it uses a typed `input` item list and a
//! richer event stream (`response.output_text.delta`, ...).

use super::common::*;
use crate::capability::{Capabilities, Support};
use crate::config::ProviderConfig;
use crate::error::{EngineError, Result};
use crate::http::{HttpClient, PreparedRequest, SseEvent};
use crate::message::{ContentPart, Role, ToolChoice};
use crate::provider::{ModelInfo, Provider, ProviderInfo, ValidationReport};
use crate::request::ChatRequest;
use crate::response::FinishReason;
use crate::stream::{EventStream, StreamEvent};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct OpenAiResponsesProvider {
    cfg: ProviderConfig,
    secret: Option<String>,
    http: HttpClient,
    info: ProviderInfo,
    caps: Capabilities,
}

impl OpenAiResponsesProvider {
    pub fn new(cfg: ProviderConfig, secret: Option<String>, http: HttpClient) -> Result<Self> {
        let info = provider_info(&cfg, "OpenAI Responses", Some("https://platform.openai.com/docs/api-reference/responses"));
        let caps = Capabilities {
            streaming: Support::Supported,
            tools: Support::Supported,
            structured_output: Support::Supported,
            reasoning: Support::Supported,
            usage_reporting: Support::Supported,
            vision: Support::Supported,
            model_listing: Support::Supported,
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

    fn build_input(&self, req: &ChatRequest) -> (String, Vec<Value>) {
        let mut instructions = String::new();
        let mut input = Vec::new();
        for m in &req.messages {
            match m.role {
                Role::System => {
                    if !instructions.is_empty() {
                        instructions.push('\n');
                    }
                    instructions.push_str(&m.text());
                }
                Role::User => {
                    let mut content = Vec::new();
                    for p in &m.content {
                        match p {
                            ContentPart::Text { text } => {
                                content.push(json!({"type": "input_text", "text": text}));
                            }
                            ContentPart::ImageUrl { url, .. } => {
                                content.push(json!({"type": "input_image", "image_url": url}));
                            }
                        }
                    }
                    input.push(json!({"role": "user", "content": content}));
                }
                Role::Assistant => {
                    let text = m.text();
                    if !text.is_empty() {
                        input.push(json!({
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": text}]
                        }));
                    }
                    for tc in &m.tool_calls {
                        input.push(json!({
                            "type": "function_call",
                            "call_id": tc.id,
                            "name": tc.name,
                            "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".into())
                        }));
                    }
                }
                Role::Tool => {
                    input.push(json!({
                        "type": "function_call_output",
                        "call_id": m.tool_call_id.clone().unwrap_or_default(),
                        "output": m.text()
                    }));
                }
            }
        }
        (instructions, input)
    }

    fn build_body(&self, req: &ChatRequest, stream: bool) -> Value {
        let (instructions, input) = self.build_input(req);
        let mut body = serde_json::Map::new();
        body.insert("model".into(), json!(req.model));
        body.insert("input".into(), Value::Array(input));
        if !instructions.is_empty() {
            body.insert("instructions".into(), json!(instructions));
        }
        body.insert("stream".into(), json!(stream));
        if let Some(m) = req.params.max_tokens {
            body.insert("max_output_tokens".into(), json!(m));
        }
        if let Some(t) = req.params.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(tp) = req.params.top_p {
            body.insert("top_p".into(), json!(tp));
        }
        if let Some(re) = &req.params.reasoning_effort {
            body.insert("reasoning".into(), json!({"effort": re}));
        }
        if req.wants_tools() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters
                    })
                })
                .collect();
            body.insert("tools".into(), Value::Array(tools));
            if let ToolChoice::Specific(name) = &req.tool_choice {
                body.insert("tool_choice".into(), json!({"type": "function", "name": name}));
            }
        }
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }
        Value::Object(body)
    }

    fn prepare(&self, body: &Value, stream: bool) -> Result<PreparedRequest> {
        let mut url = join_url(&self.cfg.base_url, "responses")?;
        let mut headers: Vec<(String, String)> = Vec::new();
        apply_static(&self.cfg, &mut headers, &mut url);
        let mut req = PreparedRequest::new(reqwest::Method::POST, url, format!("{}.responses", self.cfg.id));
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
impl Provider for OpenAiResponsesProvider {
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
            if status == 404 {
                return Err(EngineError::Unsupported("no /models endpoint".into()));
            }
            return Err(api_error(status, &headers, &text));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| EngineError::Decode(format!("invalid /models response: {e}")))?;
        let mut models = Vec::new();
        if let Some(arr) = v.get("data").and_then(Value::as_array) {
            for m in arr {
                if let Some(id) = m.get("id").and_then(Value::as_str) {
                    models.push(ModelInfo::new(id));
                }
            }
        }
        Ok(models)
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        let body = self.build_body(&req, true);
        let prepared = self.prepare(&body, true)?;
        let resp = self.http.execute(prepared, &cancel).await?;
        let status = resp.status().as_u16();
        if !(200..300).contains(&status) {
            let (status, headers, text) = read_body_limited(resp, ERROR_BODY_LIMIT).await;
            return Err(api_error(status, &headers, &text));
        }
        let sse = crate::http::sse_stream(resp);
        Ok(map_responses_stream(sse, self.cfg.id.clone(), req.model, cancel))
    }

    async fn validate(&self, cancel: &CancellationToken) -> Result<ValidationReport> {
        standard_validation(self, cancel).await
    }
}

struct ResponsesState {
    provider: String,
    model: String,
    started: bool,
    done_sent: bool,
    pending: VecDeque<Result<StreamEvent>>,
}

fn map_responses_stream(
    sse: crate::http::SseStream,
    provider: String,
    model: String,
    cancel: CancellationToken,
) -> EventStream {
    use futures::StreamExt;
    let state = ResponsesState {
        provider,
        model,
        started: false,
        done_sent: false,
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
                    None => return None,
                    Some(Err(e)) => return Some((Err(e), (sse, st, cancel))),
                    Some(Ok(ev)) => handle_responses_event(&mut st, &ev),
                }
            }
        },
    ))
}

fn handle_responses_event(st: &mut ResponsesState, ev: &SseEvent) {
    if !st.started {
        st.started = true;
        st.pending.push_back(Ok(StreamEvent::Started {
            provider: st.provider.clone(),
            model: st.model.clone(),
        }));
    }
    if ev.is_done() || ev.data.trim().is_empty() {
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
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
    match kind {
        "response.output_text.delta" => {
            if let Some(d) = v.get("delta").and_then(Value::as_str) {
                st.pending.push_back(Ok(StreamEvent::ContentDelta(d.to_string())));
            }
        }
        "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
            if let Some(d) = v.get("delta").and_then(Value::as_str) {
                st.pending.push_back(Ok(StreamEvent::ReasoningDelta(d.to_string())));
            }
        }
        "response.output_item.added" => {
            if let Some(item) = v.get("item") {
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    st.pending.push_back(Ok(StreamEvent::ToolCallDelta {
                        index: v.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize,
                        id: item.get("call_id").and_then(Value::as_str).map(str::to_string),
                        name: item.get("name").and_then(Value::as_str).map(str::to_string),
                        arguments_delta: String::new(),
                    }));
                }
            }
        }
        "response.function_call_arguments.delta" => {
            if let Some(d) = v.get("delta").and_then(Value::as_str) {
                st.pending.push_back(Ok(StreamEvent::ToolCallDelta {
                    index: v.get("output_index").and_then(Value::as_u64).unwrap_or(0) as usize,
                    id: None,
                    name: None,
                    arguments_delta: d.to_string(),
                }));
            }
        }
        "response.completed" => {
            if let Some(u) = v.get("response").and_then(|r| r.get("usage")) {
                if let Some(usage) = parse_openai_usage(u) {
                    st.pending.push_back(Ok(StreamEvent::Usage(usage.normalised())));
                }
            }
            finish(st, FinishReason::Stop);
        }
        "response.failed" | "response.incomplete" => {
            let msg = v
                .get("response")
                .and_then(|r| r.get("error"))
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("response failed");
            st.pending.push_back(Err(EngineError::Other(msg.to_string())));
        }
        "error" => {
            let msg = v.get("message").and_then(Value::as_str).unwrap_or("stream error");
            st.pending.push_back(Err(EngineError::Other(msg.to_string())));
        }
        _ => {}
    }
}

fn finish(st: &mut ResponsesState, reason: FinishReason) {
    if !st.done_sent {
        st.done_sent = true;
        st.pending.push_back(Ok(StreamEvent::Done { finish_reason: reason }));
    }
}
