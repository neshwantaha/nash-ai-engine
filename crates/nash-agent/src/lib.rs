//! The coding agent: a tool-use loop over the Nash engine.
//!
//! * [`tools`] — the filesystem/search/shell tools the model can call.
//! * [`approval`] — a policy + approver gate for side-effecting actions.
//! * [`agent`] — drives multi-step tool use, streaming events as it goes.

pub mod agent;
pub mod approval;
pub mod tools;

pub use agent::{Agent, AgentEvent, AgentOutcome, AgentSink, CollectSink, FnSink};
pub use approval::{
    ApprovalDecision, ApprovalPolicy, ApprovalRequest, ApprovalResponse, Approver,
    AutoApprover, DenyAllApprover,
};
pub use tools::{Risk, Tool, ToolContext, ToolOutcome, ToolRegistry};
