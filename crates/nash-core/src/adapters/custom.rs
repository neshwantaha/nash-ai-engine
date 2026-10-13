//! Custom endpoint adapter.
//!
//! Lets users describe a nonstandard JSON inference API without writing code:
//! a request template (with safe `{{placeholder}}` substitution), a response
//! mapping (dotted JSON paths) and a streaming format. See [`crate::template`]
//! and [`crate::config::EndpointTemplate`].

use super::common::*;
use crate::capability::{Capabilities, Support};
use crate::config::{EndpointTemplate, ProtocolKind, ProviderConfig, StreamMapping};
use crate::error::{EngineError, Result};
use crate::http::{HttpClient, PreparedRequest};
use crate::message::Role;
use crate::provider::{ModelInfo, Provider, ProviderInfo, ValidationReport};
use crate::request::ChatRequest;
use crate::response::{ChatResponse, FinishReason};
use crate::stream::{EventStream, StreamEvent};
use crate::template;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::VecDeque;
use tokio_util::sync::CancellationToken;

#[derive(Debug)]
pub struct CustomEndpointProvider {
    cfg: ProviderConfig,
    endpoint: EndpointTemplate,
    secret: Option<String>,
    http: HttpClient,
    info: ProviderInfo,
    caps: Capabilities,
}

impl CustomEndpointProvider {
    pub fn new(cfg: ProviderConfig, secret: Option<String>, http: HttpClient) -> Result<Self> {
        let endpoint = cfg
            .endpoint
            .clone()
            .ok_or_else(|| EngineError::Config("custom endpoint requires an endpoint template".into()))?;
        let info = provider_info(&cfg, &cfg.id, None);
        let mut caps = Capabilities::openai_chat_baseline().overlay(&cfg.capabilities);
        // Streaming capability is dictated by the declared stream mapping.
        caps.streaming = match &endpoint.stream {
            StreamMapping::None => {
                if caps.streaming == Support::Unknown {
                    Support::Unsupported
                } else {
                    caps.streaming
                }
            }
            _ => Support::Supported,
        };
        caps = caps.overlay(&endpoint.capabilities);
        Ok(Self {
            cfg,
            endpoint,
            secret,
            http,
            info,
            caps,
        })
    }

    fn build_body(&self, req: &ChatRequest, stream: bool) -> Value {
        let messages: Vec<Value> = req.messages.iter().map(openai_message).collect();
        let prompt = req
            .messages
            .iter()
            .rev()
            .find(|m| m.role == Role::User)
            .map(|m| m.text())
            .unwrap_or_default();
        let model = req.model.clone();
        let temperature = req.params.temperature.map(|t| json!(t)).unwrap_or(Value::Null);
        let max_tokens = req.params.max_tokens.map(|t| json!(t)).unwrap_or(Value::Null);
        let tools = openai_tools(&req.tools);
        let tool_choice = openai_tool_choice(&req.tool_choice);
        let messages_v = Value::Array(messages.clone());
        let tools_v = tools.clone();
        let tool_choice_v = tool_choice.clone();

        let vars = move |name: &str| -> Option<Value> {
            match name {
                "model" => Some(json!(model)),
                "messages" => Some(messages_v.clone()),
                "prompt" => Some(json!(prompt)),
                "stream" => Some(json!(stream)),
                "temperature" => Some(temperature.clone()),
                "max_tokens" => Some(max_tokens.clone()),
                "tools" => Some(tools_v.clone()),
                "tool_choice" => Some(tool_choice_v.clone()),
                _ => None,
            }
        };

        if let Some(template_body) = &self.endpoint.request.body {
            let mut rendered = template::render(template_body, &vars);
            if let Value::Object(map) = &mut rendered {
                for (k, v) in &self.endpoint.request.extra {
                    map.insert(k.clone(), v.clone());
                }
            }
            return rendered;
        }

        let rt = &self.endpoint.request;
        let mut body = serde_json::Map::new();
        if !rt.model_field.is_empty() {
            body.insert(rt.model_field.clone(), json!(req.model));
        }
        if !rt.messages_field.is_empty() {
            body.insert(rt.messages_field.clone(), Value::Array(messages));
        }
        if let Some(sf) = &rt.stream_field {
            body.insert(sf.clone(), json!(stream));
        }
        if let Some(t) = req.params.temperature {
            body.insert("temperature".into(), json!(t));
        }
        if let Some(m) = req.params.max_tokens {
            body.insert("max_tokens".into(), json!(m));
        }
        if req.wants_tools() {
            body.insert("tools".into(), tools);
            body.insert("tool_choice".into(), tool_choice);
        }
        for (k, v) in &rt.extra {
            body.insert(k.clone(), v.clone());
        }
        for (k, v) in &req.extra {
            body.insert(k.clone(), v.clone());
        }
        Value::Object(body)
    }

    fn prepare(&self, body: &Value, stream: bool) -> Result<PreparedRequest> {
        let method = reqwest::Method::from_bytes(self.endpoint.method.to_uppercase().as_bytes())
            .map_err(|_| EngineError::Config(format!("invalid HTTP method '{}'", self.endpoint.method)))?;
        let mut url = join_url(&self.cfg.base_url, &self.endpoint.chat_path)?;
        let mut headers: Vec<(String, String)> = Vec::new();
        apply_static(&self.cfg, &mut headers, &mut url);
        let mut req = PreparedRequest::new(method, url, format!("{}.chat", self.cfg.id));
        for (k, v) in headers {
            req = req.header(k, v);
        }
        req = crate::config::apply_auth(req, &self.cfg.auth, self.secret.as_deref())?;
        req = req.json_body(body)?;
        if stream {
            req = req.streaming();
        }
        if let Some(ms) = self.cfg.timeout_ms {
            req = req.timeout(std::time::Duration::from_millis(ms));
        }
        Ok(req)
    }

    /// Parse a complete (non-streaming) JSON response.
    fn parse_unary(&self, value: &Value, model: &str) -> ChatResponse {
        let mut resp = ChatResponse::new(self.cfg.id.clone(), model);
        if let Some(text) = template::json_path_string(value, &self.endpoint.response.text_path) {
            resp.content = text;
        }
        if let Some(p) = &self.endpoint.response.tool_calls_path {
            if let Some(tc_val) = template::json_path(value, p) {
                resp.tool_calls = parse_tool_calls_generic(tc_val);
            }
        }
        if let Some(p) = &self.endpoint.response.usage_path {
            if let Some(u) = template::json_path(value, p).and_then(parse_openai_usage) {
                resp.usage = Some(u.normalised());
            }
        }
        if let Some(p) = &self.endpoint.response.finish_reason_path {
            if let Some(fr) = template::json_path_string(value, p) {
                resp.finish_reason = FinishReason::parse(&fr);
            }
        }
        if resp.finish_reason == FinishReason::Stop && !resp.tool_calls.is_empty() {
            resp.finish_reason = FinishReason::ToolCalls;
        }
        resp
    }
}

fn parse_tool_calls_generic(value: &Value) -> Vec<crate::message::ToolCall> {
    // Try OpenAI shape first.
    let openai = parse_openai_tool_calls(value);
    if !openai.is_empty() {
        return openai;
    }
    // Fallback: {id, name, arguments}
    let mut out = Vec::new();
    if let Some(arr) = value.as_array() {
        for (i, tc) in arr.iter().enumerate() {
            let name = tc
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if name.is_empty() {
                continue;
            }
            let id = tc
                .get("id")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("call_{i}"));
            let args = tc
                .get("arguments")
                .or_else(|| tc.get("input"))
                .cloned()
                .unwrap_or(Value::Object(serde_json::Map::new()));
            let args = match args {
                Value::String(s) => serde_json::from_str(&s).unwrap_or(Value::Null),
                other => other,
            };
            out.push(crate::message::ToolCall::new(id, name, args));
        }
    }
    out
}

#[async_trait]
impl Provider for CustomEndpointProvider {
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
        let models_path = match &self.endpoint.models_path {
            Some(p) if !p.is_empty() => p.clone(),
            _ => {
                // Fall back to the configured static list.
                if !self.cfg.models.is_empty() {
                    return Ok(self.cfg.models.iter().map(ModelInfo::new).collect());
                }
                return Err(EngineError::Unsupported(
                    "no models_path configured for this custom endpoint".into(),
                ));
            }
        };
        let mut url = join_url(&self.cfg.base_url, &models_path)?;
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
            .map_err(|e| EngineError::Decode(format!("invalid models response: {e}")))?;
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

    async fn chat(&self, req: ChatRequest, cancel: CancellationToken) -> Result<ChatResponse> {
        if self.endpoint.stream != StreamMapping::None {
            return default_aggregate(self, req, cancel).await;
        }
        let body = self.build_body(&req, false);
        let prepared = self.prepare(&body, false)?;
        let resp = self.http.execute(prepared, &cancel).await?;
        let (status, headers, text) = read_body_limited(resp, 16 * 1024 * 1024).await;
        if !(200..300).contains(&status) {
            return Err(api_error(status, &headers, &text));
        }
        let v: Value = serde_json::from_str(&text)
            .map_err(|e| EngineError::Decode(format!("invalid JSON response: {e}")))?;
        Ok(self.parse_unary(&v, &req.model))
    }

    async fn stream(&self, req: ChatRequest, cancel: CancellationToken) -> Result<EventStream> {
        match &self.endpoint.stream {
            StreamMapping::None => Err(EngineError::Unsupported(format!(
                "custom endpoint '{}' declares no streaming; use chat()",
                self.cfg.id
            ))),
            mapping => {
                let body = self.build_body(&req, true);
                let prepared = self.prepare(&body, true)?;
                let resp = self.http.execute(prepared, &cancel).await?;
                let status = resp.status().as_u16();
                if !(200..300).contains(&status) {
                    let (status, headers, text) = read_body_limited(resp, ERROR_BODY_LIMIT).await;
                    return Err(api_error(status, &headers, &text));
                }
                let provider = self.cfg.id.clone();
                let model = req.model.clone();
                match mapping {
                    StreamMapping::SseJson { text_path } => {
                        let path = text_path.clone();
                        Ok(map_custom_sse(
                            crate::http::sse_stream(resp),
                            provider,
                            model,
                            move |data: &str| -> Result<Option<String>> {
                                let v: Value = serde_json::from_str(data)
                                    .map_err(|e| EngineError::Decode(format!("malformed SSE JSON: {e}")))?;
                                Ok(template::json_path_string(&v, &path))
                            },
                            cancel,
                        ))
                    }
                    StreamMapping::SseText => Ok(map_custom_sse(
                        crate::http::sse_stream(resp),
                        provider,
                        model,
                        |data: &str| -> Result<Option<String>> { Ok(Some(data.to_string())) },
                        cancel,
                    )),
                    StreamMapping::Ndjson { text_path } => {
                        let path = text_path.clone();
                        Ok(map_custom_ndjson(
                            crate::http::ndjson_stream(resp),
                            provider,
                            model,
                            move |line: &str| -> Result<Option<String>> {
                                let v: Value = serde_json::from_str(line)
                                    .map_err(|e| EngineError::Decode(format!("malformed NDJSON: {e}")))?;
                                Ok(template::json_path_string(&v, &path))
                            },
                            cancel,
                        ))
                    }
                    StreamMapping::None => unreachable!(),
                }
            }
        }
    }

    async fn validate(&self, cancel: &CancellationToken) -> Result<ValidationReport> {
        standard_validation(self, cancel).await
    }
}

async fn default_aggregate(
    provider: &dyn Provider,
    req: ChatRequest,
    cancel: CancellationToken,
) -> Result<ChatResponse> {
    use futures::StreamExt;
    let info = provider.info();
    let model = req.model.clone();
    let mut s = provider.stream(req, cancel).await?;
    let mut collector = crate::stream::StreamCollector::new();
    while let Some(ev) = s.next().await {
        collector.push(&ev?);
    }
    let mut resp = ChatResponse::new(info.id, model);
    resp.tool_calls = collector.tool_calls();
    resp.usage = collector.usage.clone();
    resp.finish_reason = collector.finish_reason.clone();
    resp.content = std::mem::take(&mut collector.content);
    resp.reasoning = std::mem::take(&mut collector.reasoning);
    Ok(resp)
}

fn map_custom_sse<F>(
    sse: crate::http::SseStream,
    provider: String,
    model: String,
    extract: F,
    cancel: CancellationToken,
) -> EventStream
where
    F: Fn(&str) -> Result<Option<String>> + Send + 'static,
{
    use futures::StreamExt;
    Box::pin(futures::stream::unfold(
        (
            sse,
            provider,
            model,
            false,
            extract,
            cancel,
            VecDeque::<Result<StreamEvent>>::new(),
        ),
        move |(mut sse, mut provider, mut model, mut started, extract, cancel, mut pending)| async move {
            loop {
                if let Some(item) = pending.pop_front() {
                    return Some((item, (sse, provider, model, started, extract, cancel, pending)));
                }
                if cancel.is_cancelled() {
                    return None;
                }
                match sse.next().await {
                    None => return None,
                    Some(Err(e)) => return Some((Err(e), (sse, provider, model, started, extract, cancel, pending))),
                    Some(Ok(ev)) => {
                        if !started {
                            started = true;
                            pending.push_back(Ok(StreamEvent::Started {
                                provider: std::mem::take(&mut provider),
                                model: std::mem::take(&mut model),
                            }));
                        }
                        if ev.is_done() {
                            continue;
                        }
                        if ev.data.trim().is_empty() {
                            continue;
                        }
                        match extract(&ev.data) {
                            Ok(Some(text)) if !text.is_empty() => {
                                pending.push_back(Ok(StreamEvent::ContentDelta(text)));
                            }
                            Ok(_) => {}
                            Err(e) => pending.push_back(Err(e)),
                        }
                    }
                }
            }
        },
    ))
}

fn map_custom_ndjson<F>(
    lines: crate::http::NdjsonStream,
    provider: String,
    model: String,
    extract: F,
    cancel: CancellationToken,
) -> EventStream
where
    F: Fn(&str) -> Result<Option<String>> + Send + 'static,
{
    use futures::StreamExt;
    Box::pin(futures::stream::unfold(
        (
            lines,
            provider,
            model,
            false,
            extract,
            cancel,
            VecDeque::<Result<StreamEvent>>::new(),
        ),
        move |(mut lines, provider, model, mut started, extract, cancel, mut pending)| async move {
            loop {
                if let Some(item) = pending.pop_front() {
                    return Some((item, (lines, provider, model, started, extract, cancel, pending)));
                }
                if cancel.is_cancelled() {
                    return None;
                }
                match lines.next().await {
                    None => return None,
                    Some(Err(e)) => return Some((Err(e), (lines, provider, model, started, extract, cancel, pending))),
                    Some(Ok(line)) => {
                        if !started {
                            started = true;
                            pending.push_back(Ok(StreamEvent::Started {
                                provider: provider.clone(),
                                model: model.clone(),
                            }));
                        }
                        if line.trim() == "[DONE]" {
                            continue;
                        }
                        match extract(&line) {
                            Ok(Some(text)) if !text.is_empty() => {
                                pending.push_back(Ok(StreamEvent::ContentDelta(text)));
                            }
                            Ok(_) => {}
                            Err(e) => pending.push_back(Err(e)),
                        }
                    }
                }
            }
        },
    ))
}

#[allow(dead_code)]
fn _assert_protocol(_: ProtocolKind) {}
