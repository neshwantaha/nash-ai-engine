//! Filesystem tools: read, write, edit and list.

use super::{arg_str, arg_str_opt, arg_u64, resolve_within, Risk, Tool, ToolContext, ToolOutcome};
use async_trait::async_trait;
use nash_core::error::{EngineError, Result};
use serde_json::json;
use similar::{ChangeTag, TextDiff};

/// Read a UTF-8 text file.
pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a UTF-8 text file from the workspace. Optionally start at a 1-based line and limit the number of lines."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path relative to the workspace root" },
                "start_line": { "type": "integer", "description": "1-based first line (optional)" },
                "max_lines": { "type": "integer", "description": "Maximum lines to return (optional)" }
            },
            "required": ["path"]
        })
    }

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let path = resolve_within(&ctx.working_dir, &arg_str(&args, "path")?)?;
        if ctx.cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let text = tokio::fs::read_to_string(&path).await.map_err(|e| {
            EngineError::Tool(format!("cannot read '{}': {e}", path.display()))
        })?;
        let start = arg_u64(&args, "start_line").unwrap_or(1).max(1) as usize;
        let max = arg_u64(&args, "max_lines").map(|n| n as usize);
        let selected: String = match max {
            Some(max) => text
                .lines()
                .skip(start - 1)
                .take(max)
                .collect::<Vec<_>>()
                .join("\n"),
            None if start > 1 => text.lines().skip(start - 1).collect::<Vec<_>>().join("\n"),
            None => text,
        };
        let (content, truncated) = ctx.truncate(selected);
        Ok(ToolOutcome {
            content,
            is_error: false,
            truncated,
        })
    }
}

/// Write (create or overwrite) a UTF-8 text file.
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Create or overwrite a UTF-8 text file in the workspace. Creates parent directories."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "content": { "type": "string" }
            },
            "required": ["path", "content"]
        })
    }

    fn risk(&self) -> Risk {
        Risk::Medium
    }

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let path = resolve_within(&ctx.working_dir, &arg_str(&args, "path")?)?;
        let content = arg_str(&args, "content")?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| EngineError::Tool(format!("cannot create '{}': {e}", parent.display())))?;
        }
        tokio::fs::write(&path, content.as_bytes())
            .await
            .map_err(|e| EngineError::Tool(format!("cannot write '{}': {e}", path.display())))?;
        Ok(ToolOutcome::ok(format!(
            "wrote {} bytes to {}",
            content.len(),
            path.display()
        )))
    }
}

/// Replace an exact substring in a file, returning a diff.
pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn name(&self) -> &str {
        "edit_file"
    }

    fn description(&self) -> &str {
        "Replace the first exact occurrence of old_string with new_string in a file. Fails if old_string is not found or is not unique."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string" },
                "old_string": { "type": "string" },
                "new_string": { "type": "string" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn risk(&self) -> Risk {
        Risk::Medium
    }

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let path = resolve_within(&ctx.working_dir, &arg_str(&args, "path")?)?;
        let old = arg_str(&args, "old_string")?;
        let new = arg_str(&args, "new_string")?;
        let text = tokio::fs::read_to_string(&path).await.map_err(|e| {
            EngineError::Tool(format!("cannot read '{}': {e}", path.display()))
        })?;
        let count = text.matches(&old).count();
        if count == 0 {
            return Ok(ToolOutcome::error(format!(
                "old_string not found in {}",
                path.display()
            )));
        }
        if count > 1 {
            return Ok(ToolOutcome::error(format!(
                "old_string is not unique in {} ({count} matches); include more context",
                path.display()
            )));
        }
        let updated = text.replacen(&old, &new, 1);
        tokio::fs::write(&path, updated.as_bytes())
            .await
            .map_err(|e| EngineError::Tool(format!("cannot write '{}': {e}", path.display())))?;

        let diff = TextDiff::from_lines(&text, &updated);
        let mut rendered = String::new();
        for change in diff.iter_all_changes() {
            let sign = match change.tag() {
                ChangeTag::Delete => "-",
                ChangeTag::Insert => "+",
                ChangeTag::Equal => continue,
            };
            rendered.push_str(&format!("{sign}{change}"));
        }
        let (content, truncated) = ctx.truncate(format!("edited {}\n{rendered}", path.display()));
        Ok(ToolOutcome {
            content,
            is_error: false,
            truncated,
        })
    }
}

/// List a directory's entries.
pub struct ListDir;

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &str {
        "list_dir"
    }

    fn description(&self) -> &str {
        "List the entries of a directory in the workspace. Directories are suffixed with '/'."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory relative to the workspace root (default '.')" }
            }
        })
    }

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let rel = arg_str_opt(&args, "path").unwrap_or_else(|| ".".to_string());
        let path = resolve_within(&ctx.working_dir, &rel)?;
        let mut entries = tokio::fs::read_dir(&path)
            .await
            .map_err(|e| EngineError::Tool(format!("cannot list '{}': {e}", path.display())))?;
        let mut lines = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|e| EngineError::Tool(e.to_string()))?
        {
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry
                .file_type()
                .await
                .map(|t| t.is_dir())
                .unwrap_or(false);
            lines.push(if is_dir { format!("{name}/") } else { name });
        }
        lines.sort();
        if lines.is_empty() {
            return Ok(ToolOutcome::ok("(empty directory)"));
        }
        let (content, truncated) = ctx.truncate(lines.join("\n"));
        Ok(ToolOutcome {
            content,
            is_error: false,
            truncated,
        })
    }
}
