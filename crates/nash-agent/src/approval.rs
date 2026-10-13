//! Approval policy and approver gate for side-effecting tool calls.
//!
//! The [`ApprovalPolicy`] decides, per tool call, whether the action is allowed
//! outright, refused, or escalated to a human via an [`Approver`]. This keeps
//! the agent UI-independent: a Tauri front-end, a CLI prompt and a test all
//! plug in their own [`Approver`].

use crate::tools::Risk;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

/// What the policy concluded for a given call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Run without asking.
    Allow,
    /// Escalate to the [`Approver`].
    Ask,
    /// Refuse; the tool is never run.
    Deny,
}

/// A request shown to an [`Approver`] before a tool runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub tool: String,
    pub args: serde_json::Value,
    pub risk: Risk,
    /// Human-readable summary of what is about to happen.
    pub reason: String,
}

/// An approver's answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalResponse {
    pub allowed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// If true, the caller may remember the choice for this tool.
    #[serde(default)]
    pub remember: bool,
}

impl ApprovalResponse {
    pub fn allow() -> Self {
        Self {
            allowed: true,
            reason: None,
            remember: false,
        }
    }

    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            allowed: false,
            reason: Some(reason.into()),
            remember: false,
        }
    }
}

/// Decides whether a pending tool call may proceed.
#[async_trait]
pub trait Approver: Send + Sync {
    async fn approve(&self, request: ApprovalRequest) -> ApprovalResponse;
}

/// Approver that allows everything it is asked about. Combine with a policy
/// that already denies the risky tools (used for unattended/trusted runs).
pub struct AutoApprover;

#[async_trait]
impl Approver for AutoApprover {
    async fn approve(&self, _req: ApprovalRequest) -> ApprovalResponse {
        ApprovalResponse {
            allowed: true,
            reason: None,
            remember: false,
        }
    }
}

/// Approver that refuses everything it is asked about.
pub struct DenyAllApprover;

#[async_trait]
impl Approver for DenyAllApprover {
    async fn approve(&self, req: ApprovalRequest) -> ApprovalResponse {
        ApprovalResponse {
            allowed: false,
            reason: Some(format!("'{}' denied by policy", req.tool)),
            remember: false,
        }
    }
}

/// Declarative rules that pre-filter tool calls before the [`Approver`] runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalPolicy {
    /// Tools that are always allowed, regardless of risk.
    pub allow_tools: HashSet<String>,
    /// Tools that are always denied, regardless of risk.
    pub deny_tools: HashSet<String>,
    /// Risk at or below this level is auto-approved without asking. `None`
    /// means everything not explicitly allowed is escalated.
    pub auto_approve_below: Option<Risk>,
    /// Risk at or above this level is denied outright (never escalated).
    pub deny_at_or_above: Option<Risk>,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            allow_tools: HashSet::new(),
            deny_tools: HashSet::new(),
            auto_approve_below: Some(Risk::Low),
            deny_at_or_above: None,
        }
    }
}

impl ApprovalPolicy {
    /// A permissive policy: nothing asks, only high-risk shell is escalated.
    pub fn permissive() -> Self {
        Self {
            auto_approve_below: Some(Risk::High),
            ..Self::default()
        }
    }

    /// A strict policy: only reads are automatic, everything else is asked.
    pub fn strict() -> Self {
        Self {
            auto_approve_below: Some(Risk::Low),
            ..Self::default()
        }
    }

    /// A locked-down policy: reads are automatic, writes and shell are denied.
    pub fn read_only() -> Self {
        Self {
            auto_approve_below: Some(Risk::Low),
            deny_at_or_above: Some(Risk::Medium),
            ..Self::default()
        }
    }

    pub fn allow_tool(mut self, name: impl Into<String>) -> Self {
        self.allow_tools.insert(name.into());
        self
    }

    pub fn deny_tool(mut self, name: impl Into<String>) -> Self {
        self.deny_tools.insert(name.into());
        self
    }

    /// Decide what should happen for a call to `tool` at `risk`.
    pub fn decide(&self, tool: &str, risk: Risk) -> ApprovalDecision {
        if self.allow_tools.contains(tool) {
            return ApprovalDecision::Allow;
        }
        if self.deny_tools.contains(tool) {
            return ApprovalDecision::Deny;
        }
        if let Some(threshold) = self.deny_at_or_above {
            if risk >= threshold {
                return ApprovalDecision::Deny;
            }
        }
        match self.auto_approve_below {
            Some(threshold) if risk <= threshold => ApprovalDecision::Allow,
            _ => ApprovalDecision::Ask,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_are_auto_allowed_by_default() {
        let p = ApprovalPolicy::default();
        assert_eq!(p.decide("read_file", Risk::Low), ApprovalDecision::Allow);
        assert_eq!(p.decide("write_file", Risk::Medium), ApprovalDecision::Ask);
        assert_eq!(p.decide("run_command", Risk::High), ApprovalDecision::Ask);
    }

    #[test]
    fn explicit_lists_win() {
        let p = ApprovalPolicy::default()
            .allow_tool("run_command")
            .deny_tool("write_file");
        assert_eq!(p.decide("run_command", Risk::High), ApprovalDecision::Allow);
        assert_eq!(p.decide("write_file", Risk::Medium), ApprovalDecision::Deny);
    }

    #[test]
    fn read_only_denies_medium_and_above() {
        let p = ApprovalPolicy::read_only();
        assert_eq!(p.decide("read_file", Risk::Low), ApprovalDecision::Allow);
        assert_eq!(p.decide("write_file", Risk::Medium), ApprovalDecision::Deny);
        assert_eq!(p.decide("run_command", Risk::High), ApprovalDecision::Deny);
    }

    #[tokio::test]
    async fn deny_all_approver_refuses() {
        let approver = DenyAllApprover;
        let resp = approver
            .approve(ApprovalRequest {
                tool: "write_file".into(),
                args: serde_json::json!({}),
                risk: Risk::Medium,
                reason: "test".into(),
            })
            .await;
        assert!(!resp.allowed);
    }
}
