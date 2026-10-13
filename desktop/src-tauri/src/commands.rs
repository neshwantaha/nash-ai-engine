//! Tauri IPC commands: the single entry point the React UI talks to.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use nash_agent::{
    Agent, AgentEvent, AgentSink, ApprovalPolicy, ApprovalRequest, ApprovalResponse, Approver,
    AutoApprover, DenyAllApprover, Risk, ToolRegistry,
};
use nash_core::config::AppConfig;
use nash_core::message::{ChatMessage, Role};
use nash_core::provider::{ModelInfo, ValidationReport};
use nash_core::request::ChatRequest;
use nash_core::response::{ChatResponse, Usage};
use nash_core::stream::StreamEvent;
use nash_core::secrets::SecretResolver;
use nash_store::{KeyringSecretStore, SecretStore, Session, StoredMessage};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;

use crate::state::{self, AppState, SERVICE};

// ---------------------------------------------------------------------------
// Paths & config
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Paths {
    pub config: String,
    pub db: String,
}

#[tauri::command]
pub fn get_paths(state: State<'_, Arc<AppState>>) -> Paths {
    Paths {
        config: state.config_path.display().to_string(),
        db: state.db_path.display().to_string(),
    }
}

#[tauri::command]
pub fn get_config(state: State<'_, Arc<AppState>>) -> AppConfig {
    state.config.lock().unwrap().clone()
}

#[tauri::command]
pub fn save_config(state: State<'_, Arc<AppState>>, config: AppConfig) -> Result<(), String> {
    config.validate().map_err(|e| e.to_string())?;
    state::write_config(&state.config_path, &config).map_err(|e| e.to_string())?;
    *state.config.lock().unwrap() = config;
    Ok(())
}

// ---------------------------------------------------------------------------
// Providers & models
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    pub id: String,
    pub display_name: Option<String>,
    pub protocol: String,
    pub base_url: String,
    pub local: bool,
    pub enabled: bool,
    /// Name of the keyring entry this provider reads its credential from.
    pub secret_ref: Option<String>,
    /// Whether a value is actually stored under `secret_ref` right now.
    pub has_secret: bool,
    pub model: Option<String>,
    pub models: Vec<String>,
    pub built: bool,
    pub build_error: Option<String>,
}

#[tauri::command]
pub fn list_providers(state: State<'_, Arc<AppState>>) -> Vec<ProviderSummary> {
    let cfg = state.config.lock().unwrap().clone();
    let engine = state.engine().ok();
    let secrets = KeyringSecretStore::new(SERVICE);
    let build_errors: HashMap<String, String> = engine
        .as_ref()
        .map(|e| e.build_errors().iter().cloned().collect())
        .unwrap_or_default();

    cfg.providers
        .iter()
        .map(|p| {
            let has_secret = match &p.secret_ref {
                Some(name) => secrets.resolve(name).ok().flatten().is_some(),
                None => false,
            };
            ProviderSummary {
                id: p.id.clone(),
                display_name: p.display_name.clone(),
                protocol: p.protocol.as_str().to_string(),
                base_url: p.base_url.clone(),
                local: p.local,
                enabled: p.enabled,
                secret_ref: p.secret_ref.clone(),
                has_secret,
                model: p.model.clone(),
                models: p.models.clone(),
                built: engine
                    .as_ref()
                    .map(|e| e.provider(&p.id).is_some())
                    .unwrap_or(false),
                build_error: build_errors.get(&p.id).cloned(),
            }
        })
        .collect()
}

#[tauri::command]
pub async fn list_models(
    state: State<'_, Arc<AppState>>,
    provider: String,
) -> Result<Vec<ModelInfo>, String> {
    let engine = state.engine()?;
    let cancel = CancellationToken::new();
    engine
        .list_models(&provider, &cancel)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn test_provider(
    state: State<'_, Arc<AppState>>,
    provider: String,
) -> Result<ValidationReport, String> {
    let engine = state.engine()?;
    let cancel = CancellationToken::new();
    engine
        .validate(&provider, &cancel)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn test_all(state: State<'_, Arc<AppState>>) -> Result<Vec<ValidationReport>, String> {
    let engine = state.engine()?;
    let cancel = CancellationToken::new();
    Ok(engine.validate_all(&cancel).await)
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatArgs {
    pub request_id: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub temperature: Option<f32>,
    #[serde(default)]
    pub max_tokens: Option<u32>,
    #[serde(default)]
    pub save: bool,
    #[serde(default)]
    pub session_id: Option<String>,
}

fn build_request(cfg: &AppConfig, args: &ChatArgs) -> ChatRequest {
    let model = args
        .model
        .clone()
        .or_else(|| {
            args.provider
                .as_ref()
                .and_then(|id| cfg.provider(id))
                .and_then(|p| p.model.clone())
        })
        .unwrap_or_default();
    let mut req = ChatRequest::new(model).messages(args.messages.clone());
    if let Some(t) = args.temperature {
        req = req.temperature(t);
    }
    if let Some(n) = args.max_tokens {
        req = req.max_tokens(n);
    }
    req
}

#[tauri::command]
pub async fn chat(
    state: State<'_, Arc<AppState>>,
    args: ChatArgs,
) -> Result<ChatResponse, String> {
    let cfg = state.snapshot();
    let req = build_request(&cfg, &args);
    let engine = state.engine()?;
    let cancel = CancellationToken::new();
    let resp = engine.chat(req, cancel).await.map_err(|e| e.to_string())?;
    persist_chat(state.inner(), &args, &resp.content, &resp.provider, &resp.model);
    Ok(resp)
}

/// A normalised streaming event forwarded to the UI.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatEvent {
    pub request_id: String,
    /// started | delta | reasoning | usage | done | error | cancelled
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ChatEvent {
    fn simple(request_id: &str, kind: &str) -> Self {
        Self {
            request_id: request_id.to_string(),
            kind: kind.to_string(),
            text: None,
            provider: None,
            model: None,
            finish_reason: None,
            usage: None,
            session_id: None,
            error: None,
        }
    }
}

fn emit_chat(app: &AppHandle, event: ChatEvent) {
    let _ = app.emit("chat://event", event);
}

#[tauri::command]
pub async fn chat_stream(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    args: ChatArgs,
) -> Result<(), String> {
    let request_id = args.request_id.clone();
    let cancel = CancellationToken::new();
    state
        .cancels
        .lock()
        .unwrap()
        .insert(request_id.clone(), cancel.clone());

    let engine = match state.engine() {
        Ok(e) => e,
        Err(e) => {
            state.cancels.lock().unwrap().remove(&request_id);
            return Err(e);
        }
    };

    let state = state.inner().clone();
    tokio::spawn(async move {
        if let Err(err) = run_stream(engine, args, cancel, app.clone(), state.clone()).await {
            let mut event = ChatEvent::simple(&request_id, "error");
            event.error = Some(err);
            emit_chat(&app, event);
        }
        state.cancels.lock().unwrap().remove(&request_id);
    });
    Ok(())
}

async fn run_stream(
    engine: nash_core::engine::Engine,
    args: ChatArgs,
    cancel: CancellationToken,
    app: AppHandle,
    state: Arc<AppState>,
) -> Result<(), String> {
    let cfg = state.snapshot();
    let req = build_request(&cfg, &args);
    let request_id = args.request_id.clone();

    let routed = engine
        .chat_stream(req, cancel.clone())
        .await
        .map_err(|e| e.to_string())?;

    let mut started = ChatEvent::simple(&request_id, "started");
    started.provider = Some(routed.provider.clone());
    started.model = Some(routed.model.clone());
    emit_chat(&app, started);

    let mut stream = routed.stream;
    let mut collected = String::new();
    let mut saw_done = false;
    while let Some(event) = stream.next().await {
        if cancel.is_cancelled() {
            emit_chat(&app, ChatEvent::simple(&request_id, "cancelled"));
            return Ok(());
        }
        match event.map_err(|e| e.to_string())? {
            StreamEvent::ContentDelta(text) => {
                collected.push_str(&text);
                let mut ev = ChatEvent::simple(&request_id, "delta");
                ev.text = Some(text);
                emit_chat(&app, ev);
            }
            StreamEvent::ReasoningDelta(text) => {
                let mut ev = ChatEvent::simple(&request_id, "reasoning");
                ev.text = Some(text);
                emit_chat(&app, ev);
            }
            StreamEvent::Usage(u) => {
                let mut ev = ChatEvent::simple(&request_id, "usage");
                ev.usage = Some(u);
                emit_chat(&app, ev);
            }
            StreamEvent::Done { finish_reason } => {
                saw_done = true;
                let session_id =
                    persist_chat(&state, &args, &collected, &routed.provider, &routed.model);
                let mut ev = ChatEvent::simple(&request_id, "done");
                ev.provider = Some(routed.provider.clone());
                ev.model = Some(routed.model.clone());
                ev.finish_reason = Some(finish_reason_name(&finish_reason));
                ev.session_id = session_id;
                emit_chat(&app, ev);
            }
            _ => {}
        }
    }
    if !saw_done {
        let session_id =
            persist_chat(&state, &args, &collected, &routed.provider, &routed.model);
        let mut ev = ChatEvent::simple(&request_id, "done");
        ev.provider = Some(routed.provider.clone());
        ev.model = Some(routed.model.clone());
        ev.finish_reason = Some("stop".to_string());
        ev.session_id = session_id;
        emit_chat(&app, ev);
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_chat(state: State<'_, Arc<AppState>>, request_id: String) -> bool {
    let cancels = state.cancels.lock().unwrap();
    if let Some(token) = cancels.get(&request_id) {
        token.cancel();
        true
    } else {
        false
    }
}

fn finish_reason_name(fr: &nash_core::response::FinishReason) -> String {
    use nash_core::response::FinishReason::*;
    match fr {
        Stop => "stop".into(),
        Length => "length".into(),
        ToolCalls => "tool_calls".into(),
        ContentFilter => "content_filter".into(),
        Other(s) => s.clone(),
    }
}

/// Persist the exchange if requested. Returns the session id when saved.
fn persist_chat(
    state: &AppState,
    args: &ChatArgs,
    assistant_text: &str,
    provider: &str,
    model: &str,
) -> Option<String> {
    if !args.save && args.session_id.is_none() {
        return None;
    }
    let session_id = match &args.session_id {
        Some(id) => id.clone(),
        None => {
            let title = args
                .messages
                .iter()
                .rev()
                .find(|m| m.role == Role::User)
                .map(|m| truncate(&m.text()))
                .unwrap_or_else(|| "chat".to_string());
            state.store.create_session(Some(title)).ok()?.id
        }
    };
    state.store.get_session(&session_id).ok().flatten()?;
    if let Some(user) = args.messages.iter().rev().find(|m| m.role == Role::User) {
        let _ = state.store.add_message(&session_id, user);
    }
    let assistant = ChatMessage::assistant(assistant_text.to_string());
    let _ = state.store.add_message(&session_id, &assistant);
    let _ = state
        .store
        .set_session_model(&session_id, Some(provider.to_string()), Some(model.to_string()));
    Some(session_id)
}

// ---------------------------------------------------------------------------
// Secrets
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_secrets() -> Result<Vec<String>, String> {
    KeyringSecretStore::new(SERVICE)
        .names()
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_secret(name: String, value: String) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("secret name must not be empty".into());
    }
    KeyringSecretStore::new(SERVICE)
        .store(&name, &value)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn remove_secret(name: String) -> Result<bool, String> {
    KeyringSecretStore::new(SERVICE)
        .remove(&name)
        .map_err(|e| e.to_string())
}

/// Store a credential for a provider under its configured `secret_ref`.
///
/// This is the "paste your API key" path: the value goes straight into the OS
/// keyring and the very next engine build uses it — no config edit needed.
#[tauri::command]
pub fn set_provider_key(
    state: State<'_, Arc<AppState>>,
    provider: String,
    value: String,
) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err("API key must not be empty".into());
    }
    let name = {
        let cfg = state.config.lock().unwrap();
        let p = cfg
            .provider(&provider)
            .ok_or_else(|| format!("unknown provider '{provider}'"))?;
        p.secret_ref
            .clone()
            .ok_or_else(|| format!("provider '{provider}' has no secret_ref configured"))?
    };
    KeyringSecretStore::new(SERVICE)
        .store(&name, &value)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tauri::command]
pub fn list_sessions(
    state: State<'_, Arc<AppState>>,
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<Vec<Session>, String> {
    state
        .store
        .list_sessions(limit.unwrap_or(50), offset.unwrap_or(0))
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_messages(
    state: State<'_, Arc<AppState>>,
    session_id: String,
) -> Result<Vec<StoredMessage>, String> {
    state
        .store
        .messages(&session_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn create_session(
    state: State<'_, Arc<AppState>>,
    title: Option<String>,
) -> Result<Session, String> {
    state
        .store
        .create_session(title)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn delete_session(state: State<'_, Arc<AppState>>, id: String) -> Result<bool, String> {
    state.store.delete_session(&id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn rename_session(
    state: State<'_, Arc<AppState>>,
    id: String,
    title: String,
) -> Result<(), String> {
    state
        .store
        .rename_session(&id, title)
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_session_pinned(
    state: State<'_, Arc<AppState>>,
    id: String,
    pinned: bool,
) -> Result<(), String> {
    state
        .store
        .set_session_pinned(&id, pinned)
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Agent
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentArgs {
    pub request_id: String,
    pub task: String,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub system: Option<String>,
    /// auto | ask | deny
    #[serde(default)]
    pub approve: Option<String>,
    #[serde(default)]
    pub max_steps: Option<usize>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentEventMsg {
    request_id: String,
    event: AgentEvent,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalEvent {
    request_id: String,
    token: String,
    tool: String,
    args: serde_json::Value,
    risk: Risk,
    reason: String,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentDone {
    request_id: String,
    result: nash_agent::AgentOutcome,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentError {
    request_id: String,
    error: String,
}

struct IpcSink {
    app: AppHandle,
    request_id: String,
}

impl AgentSink for IpcSink {
    fn event(&mut self, event: AgentEvent) {
        let _ = self.app.emit(
            "agent://event",
            AgentEventMsg {
                request_id: self.request_id.clone(),
                event,
            },
        );
    }
}

/// Approver that round-trips a decision through the UI.
struct IpcApprover {
    app: AppHandle,
    state: Arc<AppState>,
    request_id: String,
}

#[async_trait]
impl Approver for IpcApprover {
    async fn approve(&self, req: ApprovalRequest) -> ApprovalResponse {
        let token = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.state
            .approvals
            .lock()
            .unwrap()
            .insert(token.clone(), tx);

        let _ = self.app.emit(
            "agent://approval",
            ApprovalEvent {
                request_id: self.request_id.clone(),
                token: token.clone(),
                tool: req.tool.clone(),
                args: req.args.clone(),
                risk: req.risk,
                reason: req.reason.clone(),
            },
        );

        match rx.await {
            Ok(resp) => {
                self.state.approvals.lock().unwrap().remove(&token);
                resp
            }
            Err(_) => ApprovalResponse::deny("approval window closed"),
        }
    }
}

#[tauri::command]
pub fn resolve_approval(
    state: State<'_, Arc<AppState>>,
    token: String,
    allow: bool,
    reason: Option<String>,
) -> bool {
    if let Some(tx) = state.approvals.lock().unwrap().remove(&token) {
        let resp = if allow {
            ApprovalResponse::allow()
        } else {
            ApprovalResponse::deny(reason.unwrap_or_else(|| "denied by user".into()))
        };
        tx.send(resp).is_ok()
    } else {
        false
    }
}

#[tauri::command]
pub async fn agent_run(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    args: AgentArgs,
) -> Result<(), String> {
    let engine = Arc::new(state.engine()?);
    let request_id = args.request_id.clone();
    let cancel = CancellationToken::new();
    state
        .cancels
        .lock()
        .unwrap()
        .insert(request_id.clone(), cancel.clone());

    let state = state.inner().clone();
    tokio::spawn(async move {
        let mode = args
            .approve
            .clone()
            .unwrap_or_else(|| "ask".to_string());
        let policy = match mode.as_str() {
            "auto" | "all" => ApprovalPolicy::permissive(),
            "deny" | "read-only" | "readonly" => ApprovalPolicy::read_only(),
            _ => ApprovalPolicy::strict(),
        };

        let mut agent = Agent::new(engine, ToolRegistry::builtin())
            .with_policy(policy)
            .with_max_steps(args.max_steps.unwrap_or(12));
        if let Some(dir) = &args.dir {
            if !dir.trim().is_empty() {
                agent = agent.with_working_dir(dir.clone());
            }
        }
        if let Some(m) = &args.model {
            agent = agent.with_model(m.clone());
        }
        if let Some(s) = &args.system {
            agent = agent.with_system_prompt(s.clone());
        }

        let approver: Box<dyn Approver> = match mode.as_str() {
            "auto" | "all" => Box::new(AutoApprover),
            "deny" | "read-only" | "readonly" => Box::new(DenyAllApprover),
            _ => Box::new(IpcApprover {
                app: app.clone(),
                state: state.clone(),
                request_id: request_id.clone(),
            }),
        };

        let mut sink = IpcSink {
            app: app.clone(),
            request_id: request_id.clone(),
        };

        let outcome = agent
            .run(
                vec![ChatMessage::user(args.task.clone())],
                approver.as_ref(),
                &mut sink,
                cancel,
            )
            .await;

        match outcome {
            Ok(result) => {
                if let Ok(session) = state.store.create_session(Some(truncate(&args.task))) {
                    for msg in &result.messages {
                        let _ = state.store.add_message(&session.id, msg);
                    }
                }
                let _ = app.emit(
                    "agent://done",
                    AgentDone {
                        request_id: request_id.clone(),
                        result,
                    },
                );
            }
            Err(e) => {
                let _ = app.emit(
                    "agent://error",
                    AgentError {
                        request_id: request_id.clone(),
                        error: e.to_string(),
                    },
                );
            }
        }
        state.cancels.lock().unwrap().remove(&request_id);
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate(text: &str) -> String {
    let one: String = text
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .chars()
        .take(48)
        .collect();
    if one.is_empty() {
        "untitled".to_string()
    } else {
        one
    }
}
