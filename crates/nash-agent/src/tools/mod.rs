//! Tool abstraction and registry.
//!
//! Every tool is described to the model by a JSON-schema function definition
//! and executed with a [`ToolContext`] that carries the workspace root and a
//! cancellation token. Filesystem tools refuse to escape the workspace root.

pub mod fs;
pub mod search;
pub mod shell;

use async_trait::async_trait;
use nash_core::error::{EngineError, Result};
use nash_core::message::ToolDefinition;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// How dangerous a tool is. Drives the default approval policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Risk {
    /// Reads only; no side effects.
    Low,
    /// Writes to the workspace (files, edits).
    Medium,
    /// Executes arbitrary commands or touches things outside the workspace.
    High,
}

/// Ambient information handed to every tool invocation.
#[derive(Clone)]
pub struct ToolContext {
    /// The workspace root; filesystem tools are confined to it.
    pub working_dir: PathBuf,
    pub cancel: CancellationToken,
    /// Upper bound on returned content, in bytes.
    pub max_output_bytes: usize,
}

impl ToolContext {
    pub fn new(working_dir: impl Into<PathBuf>, cancel: CancellationToken) -> Self {
        Self {
            working_dir: working_dir.into(),
            cancel,
            max_output_bytes: 64 * 1024,
        }
    }

    /// Truncate `text` to `max_output_bytes`, appending a marker.
    pub fn truncate(&self, text: String) -> (String, bool) {
        if text.len() <= self.max_output_bytes {
            return (text, false);
        }
        let mut cut = self.max_output_bytes;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        let mut out = text[..cut].to_string();
        out.push_str("\n… [truncated]");
        (out, true)
    }
}

/// The result of running a tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub content: String,
    #[serde(default)]
    pub is_error: bool,
    #[serde(default)]
    pub truncated: bool,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            truncated: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            truncated: false,
        }
    }

    pub fn truncated(mut self) -> Self {
        self.truncated = true;
        self
    }
}

/// A callable tool.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Name as it appears in the model's function schema.
    fn name(&self) -> &str;
    /// Human-readable description for the model.
    fn description(&self) -> &str;
    /// JSON Schema for the arguments object.
    fn parameters(&self) -> serde_json::Value;
    /// Risk level (defaults to [`Risk::Low`]).
    fn risk(&self) -> Risk {
        Risk::Low
    }
    /// Execute the tool.
    async fn call(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome>;

    /// Convenience: the model-facing definition.
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(self.name(), self.description(), self.parameters())
    }
}

/// An ordered, name-keyed collection of tools.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// The default tool set: filesystem, search and shell.
    pub fn builtin() -> Self {
        let mut reg = Self::new();
        reg.insert(Arc::new(fs::ReadFile));
        reg.insert(Arc::new(fs::WriteFile));
        reg.insert(Arc::new(fs::EditFile));
        reg.insert(Arc::new(fs::ListDir));
        reg.insert(Arc::new(search::SearchText));
        reg.insert(Arc::new(search::GlobFiles));
        reg.insert(Arc::new(shell::RunCommand));
        reg
    }

    pub fn insert(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn remove(&mut self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).cloned()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.keys().cloned().collect()
    }

    /// Model-facing definitions, in stable (name) order.
    pub fn definitions(&self) -> Vec<ToolDefinition> {
        self.tools.values().map(|t| t.definition()).collect()
    }

    /// Execute a tool by name.
    pub async fn call(
        &self,
        name: &str,
        args: serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutcome> {
        match self.tools.get(name) {
            Some(tool) => tool.call(args, ctx).await,
            None => Err(EngineError::Tool(format!("unknown tool '{name}'"))),
        }
    }
}

/// Resolve `candidate` against `root`, refusing to escape it.
///
/// The check is lexical (the target may not exist yet, e.g. for writes): `.`
/// components are dropped and `..` pops, and the result must stay under root.
pub fn resolve_within(root: &Path, candidate: &str) -> Result<PathBuf> {
    let root_abs = root
        .canonicalize()
        .unwrap_or_else(|_| root.to_path_buf());
    let candidate_path = Path::new(candidate);
    let joined = if candidate_path.is_absolute() {
        candidate_path.to_path_buf()
    } else {
        root_abs.join(candidate_path)
    };
    let normalized = lexical_normalize(&joined);
    if !normalized.starts_with(&root_abs) {
        return Err(EngineError::Security(format!(
            "path '{candidate}' escapes the workspace root"
        )));
    }
    Ok(normalized)
}

fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Read a required string argument.
pub(crate) fn arg_str(args: &serde_json::Value, key: &str) -> Result<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| EngineError::Tool(format!("missing string argument '{key}'")))
}

/// Read an optional string argument.
pub(crate) fn arg_str_opt(args: &serde_json::Value, key: &str) -> Option<String> {
    args.get(key).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Read an optional unsigned argument.
pub(crate) fn arg_u64(args: &serde_json::Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_blocks_escape() {
        let root = std::env::temp_dir();
        assert!(resolve_within(&root, "sub/file.txt").is_ok());
        assert!(resolve_within(&root, "../../etc/passwd").is_err());
    }

    #[test]
    fn normalize_removes_dot_segments() {
        let p = lexical_normalize(Path::new("/a/./b/../c"));
        assert_eq!(p, PathBuf::from("/a/c"));
    }

    #[test]
    fn builtin_registry_is_populated() {
        let reg = ToolRegistry::builtin();
        assert!(reg.contains("read_file"));
        assert!(reg.contains("write_file"));
        assert!(reg.contains("run_command"));
        assert_eq!(reg.definitions().len(), reg.len());
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let reg = ToolRegistry::new();
        let ctx = ToolContext::new(std::env::temp_dir(), CancellationToken::new());
        assert!(reg.call("nope", serde_json::json!({}), &ctx).await.is_err());
    }
}
