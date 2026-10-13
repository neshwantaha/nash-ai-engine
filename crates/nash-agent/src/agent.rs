//! The agent loop.
//!
//! [`Agent`] drives a chat model through repeated tool-use turns: it streams a
//! completion, executes any requested tools (subject to the approval gate),
//! feeds the results back, and repeats until the model stops asking for tools
//! or the step budget is exhausted. Every intermediate step is surfaced as an
//! [`AgentEvent`] so a CLI or GUI can render progress live.

use crate::approval::{ApprovalDecision, ApprovalPolicy, ApprovalRequest, Approver};
use crate::tools::{Risk, ToolContext, ToolRegistry};
use futures::StreamExt;
use nash_core::engine::Engine;
use nash_core::error::{EngineError, Result};
use nash_core::message::ChatMessage;
use nash_core::request::ChatRequest;
use nash_core::response::{FinishReason, Usage};
use nash_core::stream::{StreamCollector, StreamEvent};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Progress events emitted during a run.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// A new model turn has begun.
    Step { index: usize },
    /// Streaming assistant text.
    TextDelta { text: String },
    /// Streaming reasoning/thinking text.
    ReasoningDelta { text: String },
    /// The model requested a tool.
    ToolCall {
        id: String,
        name: String,
        arguments: serde_json::Value,
        risk: Risk,
    },
    /// A tool finished.
    ToolResult {
        id: String,
        name: String,
        content: String,
        is_error: bool,
    },
    /// A tool was refused by the approval gate.
    ApprovalDenied {
        id: String,
        name: String,
        reason: String,
    },
    /// The run is complete.
    Finished {
        steps: usize,
        tool_calls: usize,
        denials: usize,
        finish_reason: FinishReason,
    },
}

/// Receives [`AgentEvent`]s. Implemented for `Vec<AgentEvent>` for tests.
pub trait AgentSink: Send {
    fn event(&mut self, event: AgentEvent);
}

impl AgentSink for Vec<AgentEvent> {
    fn event(&mut self, event: AgentEvent) {
        self.push(event);
    }
}

impl AgentSink for () {
    fn event(&mut self, _event: AgentEvent) {}
}

/// A sink that forwards events to a closure.
pub struct FnSink<F: FnMut(AgentEvent)> {
    f: F,
}

impl<F: FnMut(AgentEvent)> FnSink<F> {
    pub fn new(f: F) -> Self {
        Self { f }
    }
}

impl<F: FnMut(AgentEvent) + Send> AgentSink for FnSink<F> {
    fn event(&mut self, event: AgentEvent) {
        (self.f)(event);
    }
}

/// A sink that simply collects events.
#[derive(Default)]
pub struct CollectSink(pub Vec<AgentEvent>);

impl AgentSink for CollectSink {
    fn event(&mut self, event: AgentEvent) {
        self.0.push(event);
    }
}

/// The result of a completed (or budget-exhausted) run.
#[derive(Debug, Clone, Serialize)]
pub struct AgentOutcome {
    /// The final assistant text.
    pub content: String,
    pub reasoning: String,
    /// The provider/model that served the final turn.
    pub provider: String,
    pub model: String,
    pub steps: usize,
    pub tool_calls: usize,
    pub denials: usize,
    pub finish_reason: FinishReason,
    pub usage: Option<Usage>,
    /// The full conversation, including tool turns, for persistence.
    pub messages: Vec<ChatMessage>,
}

/// The tool-using agent.
pub struct Agent {
    engine: Arc<Engine>,
    tools: ToolRegistry,
    policy: ApprovalPolicy,
    system_prompt: Option<String>,
    model: Option<String>,
    max_steps: usize,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    working_dir: PathBuf,
    max_output_bytes: usize,
}

impl Agent {
    pub fn new(engine: Arc<Engine>, tools: ToolRegistry) -> Self {
        let working_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            engine,
            tools,
            policy: ApprovalPolicy::default(),
            system_prompt: None,
            model: None,
            max_steps: 12,
            temperature: None,
            max_tokens: None,
            working_dir,
            max_output_bytes: 64 * 1024,
        }
    }

    pub fn with_policy(mut self, policy: ApprovalPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    pub fn with_max_steps(mut self, max_steps: usize) -> Self {
        self.max_steps = max_steps.max(1);
        self
    }

    pub fn with_working_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.working_dir = dir.into();
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    pub fn with_max_output_bytes(mut self, bytes: usize) -> Self {
        self.max_output_bytes = bytes.max(1024);
        self
    }

    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    pub fn policy(&self) -> &ApprovalPolicy {
        &self.policy
    }

    /// Run the loop. `messages` is the seed conversation (system/user turns);
    /// assistant and tool turns are appended internally.
    pub async fn run(
        &self,
        messages: Vec<ChatMessage>,
        approver: &dyn Approver,
        sink: &mut dyn AgentSink,
        cancel: CancellationToken,
    ) -> Result<AgentOutcome> {
        let mut conversation = self.seed(messages);
        let mut ctx = ToolContext::new(self.working_dir.clone(), cancel.clone());
        ctx.max_output_bytes = self.max_output_bytes;

        let mut total_tool_calls = 0usize;
        let mut denials = 0usize;
        let mut usage_total = Usage::default();
        let mut saw_usage = false;
        let mut last_provider = String::new();
        let mut last_model = self.model.clone().unwrap_or_default();

        for step in 0..self.max_steps {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            sink.event(AgentEvent::Step { index: step });

            let mut req = ChatRequest::new(self.model.clone().unwrap_or_default())
                .messages(conversation.clone());
            for def in self.tools.definitions() {
                req = req.tool(def);
            }
            if let Some(t) = self.temperature {
                req = req.temperature(t);
            }
            if let Some(n) = self.max_tokens {
                req = req.max_tokens(n);
            }

            let routed = self.engine.chat_stream(req, cancel.clone()).await?;
            last_provider = routed.provider.clone();
            last_model = routed.model.clone();

            let mut collector = StreamCollector::new();
            let mut stream = routed.stream;
            while let Some(event) = stream.next().await {
                if cancel.is_cancelled() {
                    return Err(EngineError::Cancelled);
                }
                let event = event?;
                match &event {
                    StreamEvent::ContentDelta(text) => {
                        sink.event(AgentEvent::TextDelta { text: text.clone() });
                    }
                    StreamEvent::ReasoningDelta(text) => {
                        sink.event(AgentEvent::ReasoningDelta { text: text.clone() });
                    }
                    _ => {}
                }
                collector.push(&event);
            }

            if let Some(u) = &collector.usage {
                usage_total.prompt_tokens += u.prompt_tokens;
                usage_total.completion_tokens += u.completion_tokens;
                usage_total.reasoning_tokens = Some(
                    usage_total.reasoning_tokens.unwrap_or(0)
                        + u.reasoning_tokens.unwrap_or(0),
                );
                saw_usage = true;
            }

            let tool_calls = collector.tool_calls();
            if tool_calls.is_empty() {
                usage_total.total_tokens =
                    usage_total.prompt_tokens + usage_total.completion_tokens;
                sink.event(AgentEvent::Finished {
                    steps: step + 1,
                    tool_calls: total_tool_calls,
                    denials,
                    finish_reason: collector.finish_reason.clone(),
                });
                return Ok(AgentOutcome {
                    content: collector.content.clone(),
                    reasoning: collector.reasoning.clone(),
                    provider: last_provider,
                    model: last_model,
                    steps: step + 1,
                    tool_calls: total_tool_calls,
                    denials,
                    finish_reason: collector.finish_reason.clone(),
                    usage: saw_usage.then_some(usage_total),
                    messages: conversation,
                });
            }

            // Record the assistant turn (content + requested calls).
            let mut assistant = ChatMessage::assistant(collector.content.clone());
            assistant.tool_calls = tool_calls.clone();
            conversation.push(assistant);

            for call in &tool_calls {
                total_tool_calls += 1;
                let risk = self
                    .tools
                    .get(&call.name)
                    .map(|t| t.risk())
                    .unwrap_or(Risk::High);
                sink.event(AgentEvent::ToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    risk,
                });

                let outcome = match self.policy.decide(&call.name, risk) {
                    ApprovalDecision::Allow => {
                        self.execute(&call.name, &call.arguments, &ctx).await
                    }
                    ApprovalDecision::Deny => {
                        denials += 1;
                        let reason = format!("'{}' denied by policy", call.name);
                        sink.event(AgentEvent::ApprovalDenied {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            reason: reason.clone(),
                        });
                        Err(EngineError::Denied(reason))
                    }
                    ApprovalDecision::Ask => {
                        let resp = approver
                            .approve(ApprovalRequest {
                                tool: call.name.clone(),
                                args: call.arguments.clone(),
                                risk,
                                reason: format!("run tool '{}'", call.name),
                            })
                            .await;
                        if resp.allowed {
                            self.execute(&call.name, &call.arguments, &ctx).await
                        } else {
                            denials += 1;
                            let reason = resp
                                .reason
                                .unwrap_or_else(|| format!("'{}' denied by user", call.name));
                            sink.event(AgentEvent::ApprovalDenied {
                                id: call.id.clone(),
                                name: call.name.clone(),
                                reason: reason.clone(),
                            });
                            Err(EngineError::Denied(reason))
                        }
                    }
                };

                let (content, is_error) = match outcome {
                    Ok(o) => (o.content, o.is_error),
                    Err(EngineError::Denied(reason)) => (reason, true),
                    Err(e) => (format!("error: {e}"), true),
                };
                sink.event(AgentEvent::ToolResult {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    content: content.clone(),
                    is_error,
                });

                let mut tool_msg = ChatMessage::tool_result(call.id.clone(), content);
                if is_error {
                    tool_msg.name = Some(call.name.clone());
                }
                conversation.push(tool_msg);
            }
        }

        // Step budget exhausted.
        usage_total.total_tokens = usage_total.prompt_tokens + usage_total.completion_tokens;
        sink.event(AgentEvent::Finished {
            steps: self.max_steps,
            tool_calls: total_tool_calls,
            denials,
            finish_reason: FinishReason::Length,
        });
        Ok(AgentOutcome {
            content: format!(
                "stopped after {} steps without a final answer",
                self.max_steps
            ),
            reasoning: String::new(),
            provider: last_provider,
            model: last_model,
            steps: self.max_steps,
            tool_calls: total_tool_calls,
            denials,
            finish_reason: FinishReason::Length,
            usage: saw_usage.then_some(usage_total),
            messages: conversation,
        })
    }

    async fn execute(
        &self,
        name: &str,
        args: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<crate::tools::ToolOutcome> {
        self.tools.call(name, args.clone(), ctx).await
    }

    fn seed(&self, mut messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
        if let Some(sys) = &self.system_prompt {
            let has_system = messages
                .first()
                .map(|m| m.role == nash_core::message::Role::System)
                .unwrap_or(false);
            if !has_system {
                messages.insert(0, ChatMessage::system(sys.clone()));
            }
        }
        messages
    }
}
