//! Command implementations for the `nash` CLI.

use crate::paths;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use futures::StreamExt;
use nash_agent::{
    Agent, AgentEvent, AgentSink, ApprovalPolicy, ApprovalRequest, ApprovalResponse, Approver,
    AutoApprover, DenyAllApprover, ToolRegistry,
};
use nash_core::config::AppConfig;
use nash_core::engine::Engine;
use nash_core::message::ChatMessage;
use nash_core::provider::CheckStatus;
use nash_core::request::ChatRequest;
use nash_core::stream::StreamEvent;
use nash_store::{KeyringSecretStore, SecretStore, Session, Store};
use std::io::Write;
use std::path::Path;
use std::sync::Arc;

/// Service name grouping this app's credentials in the OS keyring.
const SERVICE: &str = "nash-ai-engine";

/// Shared context derived from global CLI options.
pub struct Ctx {
    pub config_path: std::path::PathBuf,
    pub db_path: std::path::PathBuf,
}

impl Ctx {
    pub fn new(config: Option<std::path::PathBuf>, db: Option<std::path::PathBuf>) -> Self {
        Self {
            config_path: config.unwrap_or_else(paths::default_config_path),
            db_path: db.unwrap_or_else(paths::default_db_path),
        }
    }

    pub fn load_config(&self) -> Result<AppConfig> {
        if self.config_path.exists() {
            let text = std::fs::read_to_string(&self.config_path)?;
            Ok(AppConfig::from_json(&text)?)
        } else {
            Ok(AppConfig::default())
        }
    }

    pub fn save_config(&self, cfg: &AppConfig) -> Result<()> {
        if let Some(parent) = self.config_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.config_path, cfg.to_json_pretty()?)?;
        Ok(())
    }

    fn secrets(&self) -> KeyringSecretStore {
        KeyringSecretStore::new(SERVICE)
    }

    pub fn engine(&self) -> Result<Engine> {
        let cfg = self.load_config()?;
        Ok(Engine::new(cfg, Box::new(self.secrets()))?)
    }

    pub fn store(&self) -> Result<Store> {
        Ok(Store::open(&self.db_path)?)
    }
}

// ---------------------------------------------------------------------------
// providers / models / test
// ---------------------------------------------------------------------------

pub fn providers(ctx: &Ctx) -> Result<()> {
    let cfg = ctx.load_config()?;
    if cfg.providers.is_empty() {
        println!("No providers configured.");
        println!("Edit {} to add one.", ctx.config_path.display());
        return Ok(());
    }
    let default = cfg.default_provider.as_deref();
    for p in &cfg.providers {
        let marker = if Some(p.id.as_str()) == default { "*" } else { " " };
        let local = if p.local { "local" } else { "remote" };
        let model = p.model.as_deref().unwrap_or("(no model set)");
        println!(
            "{marker} {:<20} {:<18} {:<7} {}  [{}]",
            p.id,
            p.protocol.as_str(),
            local,
            p.base_url,
            model
        );
        if !p.enabled {
            println!("    (disabled)");
        }
    }
    Ok(())
}

pub async fn models(ctx: &Ctx, provider: &str) -> Result<()> {
    let engine = ctx.engine()?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let models = engine
        .list_models(provider, &cancel)
        .await
        .map_err(|e| anyhow!("{e}"))?;
    if models.is_empty() {
        println!("(no models advertised by '{provider}')");
    }
    for m in models {
        match m.context_window {
            Some(c) => println!("{:<40} context {}", m.id, c),
            None => println!("{}", m.id),
        }
    }
    Ok(())
}

pub async fn test(ctx: &Ctx, provider: Option<&str>, all: bool) -> Result<()> {
    let engine = ctx.engine()?;
    let cancel = tokio_util::sync::CancellationToken::new();
    let reports = match provider {
        Some(id) if !all => {
            vec![engine
                .validate(id, &cancel)
                .await
                .map_err(|e| anyhow!("{e}"))?]
        }
        _ => engine.validate_all(&cancel).await,
    };
    let mut any_fail = false;
    for report in reports {
        println!("{}", report.provider);
        for check in &report.checks {
            let sym = match check.status {
                CheckStatus::Pass => "ok",
                CheckStatus::Warn => "warn",
                CheckStatus::Fail => {
                    any_fail = true;
                    "FAIL"
                }
                CheckStatus::Skip => "skip",
            };
            let detail = check
                .detail
                .as_deref()
                .map(|d| format!("  {d}"))
                .unwrap_or_default();
            let latency = check
                .latency_ms
                .map(|m| format!(" ({m} ms)"))
                .unwrap_or_default();
            println!("  [{sym:<4}] {}{latency}{detail}", check.name);
        }
        println!();
    }
    if any_fail {
        anyhow::bail!("one or more checks failed");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// chat
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
pub async fn chat(
    ctx: &Ctx,
    prompt: &str,
    provider: Option<&str>,
    model: Option<&str>,
    system: Option<&str>,
    stream: bool,
    session: Option<&str>,
    save: bool,
) -> Result<()> {
    let cfg = ctx.load_config()?;
    let model = match model {
        Some(m) => Some(m.to_string()),
        None => provider
            .and_then(|id| cfg.provider(id))
            .and_then(|p| p.model.clone()),
    };

    let mut messages: Vec<ChatMessage> = Vec::new();
    if let Some(sys) = system {
        messages.push(ChatMessage::system(sys));
    }

    let store = ctx.store().ok();
    let mut active_session: Option<Session> = None;
    if let Some(id) = session {
        if let Some(store) = &store {
            let sess = store
                .get_session(id)?
                .ok_or_else(|| anyhow!("session '{id}' not found"))?;
            for m in store.messages(id)? {
                messages.push(m.message);
            }
            active_session = Some(sess);
        }
    } else if save {
        if let Some(store) = &store {
            active_session = Some(store.create_session(Some(truncate_title(prompt)))?);
        }
    }

    messages.push(ChatMessage::user(prompt));
    let req = ChatRequest::new(model.unwrap_or_default()).messages(messages);

    let engine = ctx.engine()?;
    let cancel = cancelling_runtime();

    let response_text = if stream {
        let routed = engine
            .chat_stream(req, cancel.clone())
            .await
            .map_err(|e| anyhow!("{e}"))?;
        let mut collected = String::new();
        let mut out = std::io::stdout();
        let mut events = routed.stream;
        while let Some(ev) = events.next().await {
            match ev.map_err(|e| anyhow!("{e}"))? {
                StreamEvent::ContentDelta(text) => {
                    collected.push_str(&text);
                    out.write_all(text.as_bytes())?;
                    out.flush()?;
                }
                StreamEvent::ReasoningDelta(_) => {}
                _ => {}
            }
        }
        println!();
        collected
    } else {
        let resp = engine
            .chat(req, cancel.clone())
            .await
            .map_err(|e| anyhow!("{e}"))?;
        println!("{}", resp.content);
        resp.content
    };

    if let (Some(store), Some(session)) = (store.as_ref(), active_session.as_ref()) {
        store.add_message(&session.id, &ChatMessage::user(prompt))?;
        store.add_message(&session.id, &ChatMessage::assistant(&response_text))?;
        eprintln!("[session {}]", session.id);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// agent
// ---------------------------------------------------------------------------

/// Interactive approver that asks on the terminal.
struct CliApprover;

#[async_trait]
impl Approver for CliApprover {
    async fn approve(&self, req: ApprovalRequest) -> ApprovalResponse {
        eprintln!(
            "\n[nash] approve tool '{}' (risk {:?})?\n  args: {}\n[y/N] ",
            req.tool, req.risk, req.args
        );
        let mut line = String::new();
        let read_ok = std::io::stdin().read_line(&mut line).is_ok();
        let yes = read_ok && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes");
        ApprovalResponse {
            allowed: yes,
            reason: if yes {
                None
            } else {
                Some("denied by user".into())
            },
            remember: false,
        }
    }
}

/// Streams agent progress: model text to stdout, tool activity to stderr.
struct ConsoleSink;

impl AgentSink for ConsoleSink {
    fn event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::Step { index } => {
                if index > 0 {
                    eprintln!("\n-- step {index} --");
                }
            }
            AgentEvent::TextDelta { text } => {
                print!("{text}");
                let _ = std::io::stdout().flush();
            }
            AgentEvent::ReasoningDelta { text } => {
                eprint!("{text}");
            }
            AgentEvent::ToolCall {
                name, arguments, ..
            } => {
                eprintln!("\n[tool] {name} {arguments}");
            }
            AgentEvent::ToolResult {
                name,
                content,
                is_error,
                ..
            } => {
                let tag = if is_error { "tool error" } else { "tool" };
                eprintln!("[{tag}: {name}]\n{}", first_lines(&content, 6));
            }
            AgentEvent::ApprovalDenied { name, reason, .. } => {
                eprintln!("[denied] {name}: {reason}");
            }
            AgentEvent::Finished {
                steps,
                tool_calls,
                denials,
                ..
            } => {
                println!();
                eprintln!("[done] {steps} step(s), {tool_calls} tool call(s), {denials} denied");
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn agent(
    ctx: &Ctx,
    task: &str,
    dir: &Path,
    model: Option<&str>,
    system: Option<&str>,
    approve: &str,
    max_steps: usize,
) -> Result<()> {
    let engine = ctx.engine()?;
    let policy = match approve {
        "auto" | "all" => ApprovalPolicy::permissive(),
        "deny" | "none" | "read-only" | "readonly" => ApprovalPolicy::read_only(),
        _ => ApprovalPolicy::strict(),
    };

    let mut agent = Agent::new(Arc::new(engine), ToolRegistry::builtin())
        .with_working_dir(dir)
        .with_max_steps(max_steps)
        .with_policy(policy);
    if let Some(m) = model {
        agent = agent.with_model(m);
    }
    if let Some(s) = system {
        agent = agent.with_system_prompt(s);
    }

    let cancel = cancelling_runtime();
    let auto = AutoApprover;
    let deny = DenyAllApprover;
    let cli = CliApprover;
    let approver: &dyn Approver = match approve {
        "auto" | "all" => &auto,
        "deny" | "none" | "read-only" => &deny,
        _ => &cli,
    };

    let mut sink = ConsoleSink;
    let outcome = agent
        .run(vec![ChatMessage::user(task)], approver, &mut sink, cancel)
        .await
        .map_err(|e| anyhow!("{e}"))?;

    if let Ok(store) = ctx.store() {
        let session = store.create_session(Some(truncate_title(task)))?;
        for m in &outcome.messages {
            store.add_message(&session.id, m)?;
        }
        eprintln!("[session {}]", session.id);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// config
// ---------------------------------------------------------------------------

pub fn config_show(ctx: &Ctx) -> Result<()> {
    println!("{}", ctx.load_config()?.to_json_pretty()?);
    Ok(())
}

pub fn config_paths(ctx: &Ctx) -> Result<()> {
    println!("config: {}", ctx.config_path.display());
    println!("db:     {}", ctx.db_path.display());
    Ok(())
}

pub fn config_init(ctx: &Ctx, force: bool) -> Result<()> {
    if ctx.config_path.exists() && !force {
        anyhow::bail!(
            "{} already exists (use --force to overwrite)",
            ctx.config_path.display()
        );
    }
    ctx.save_config(&AppConfig::starter())?;
    println!("wrote {}", ctx.config_path.display());
    Ok(())
}

pub fn config_set_default(ctx: &Ctx, provider: &str) -> Result<()> {
    let mut cfg = ctx.load_config()?;
    if cfg.provider(provider).is_none() {
        anyhow::bail!("no provider '{provider}' in configuration");
    }
    cfg.default_provider = Some(provider.to_string());
    ctx.save_config(&cfg)?;
    println!("default provider set to '{provider}'");
    Ok(())
}

// ---------------------------------------------------------------------------
// secrets
// ---------------------------------------------------------------------------

pub fn secret_set(ctx: &Ctx, name: &str, value: Option<&str>) -> Result<()> {
    let value = match value {
        Some(v) => v.to_string(),
        None => prompt_secret(name)?,
    };
    ctx.secrets().store(name, &value)?;
    println!("stored secret '{name}'");
    Ok(())
}

pub fn secret_list(ctx: &Ctx) -> Result<()> {
    let names = ctx.secrets().names()?;
    if names.is_empty() {
        println!("(no secrets stored)");
    }
    for n in names {
        println!("{n}");
    }
    Ok(())
}

pub fn secret_remove(ctx: &Ctx, name: &str) -> Result<()> {
    if ctx.secrets().remove(name)? {
        println!("removed secret '{name}'");
    } else {
        println!("no secret named '{name}'");
    }
    Ok(())
}

fn prompt_secret(name: &str) -> Result<String> {
    eprint!("enter value for secret '{name}': ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let value = line.trim_end().to_string();
    if value.is_empty() {
        anyhow::bail!("secret value must not be empty");
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// sessions
// ---------------------------------------------------------------------------

pub fn sessions_list(ctx: &Ctx, limit: i64) -> Result<()> {
    let store = ctx.store()?;
    let sessions = store.list_sessions(limit, 0)?;
    if sessions.is_empty() {
        println!("(no sessions)");
        return Ok(());
    }
    for s in sessions {
        let marker = if s.pinned { "*" } else { " " };
        let title = s.title.as_deref().unwrap_or("(none)");
        let provider = s.provider.as_deref().unwrap_or("-");
        let model = s.model.as_deref().unwrap_or("-");
        println!(
            "{marker} {:<38} {:<16} {:<18} {}",
            s.id, provider, model, title
        );
    }
    Ok(())
}

pub fn sessions_show(ctx: &Ctx, id: &str) -> Result<()> {
    let store = ctx.store()?;
    let session = store
        .get_session(id)?
        .ok_or_else(|| anyhow!("session '{id}' not found"))?;
    println!("id:       {}", session.id);
    println!("title:    {}", session.title.as_deref().unwrap_or("(none)"));
    println!("provider: {}", session.provider.as_deref().unwrap_or("-"));
    println!("model:    {}", session.model.as_deref().unwrap_or("-"));
    println!("created:  {}", session.created_at);
    println!("updated:  {}", session.updated_at);
    println!("---");
    for m in store.messages(id)? {
        println!("[{}] {}", m.message.role.as_str(), m.message.text());
    }
    Ok(())
}

pub fn sessions_delete(ctx: &Ctx, id: &str) -> Result<()> {
    let store = ctx.store()?;
    if store.delete_session(id)? {
        println!("deleted session {id}");
    } else {
        println!("no session '{id}'");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// A cancellation token wired to Ctrl+C.
fn cancelling_runtime() -> tokio_util::sync::CancellationToken {
    let cancel = tokio_util::sync::CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            c.cancel();
        }
    });
    cancel
}

fn truncate_title(prompt: &str) -> String {
    let one_line: String = prompt.lines().next().unwrap_or("").trim().to_string();
    if one_line.is_empty() {
        "untitled".to_string()
    } else if one_line.chars().count() > 48 {
        let s: String = one_line.chars().take(48).collect();
        format!("{s}…")
    } else {
        one_line
    }
}

fn first_lines(text: &str, n: usize) -> String {
    text.lines().take(n).collect::<Vec<_>>().join("\n")
}
