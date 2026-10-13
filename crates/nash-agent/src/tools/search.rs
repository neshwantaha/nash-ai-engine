//! Search tools: regex content search and glob file matching.

use super::{arg_str, arg_str_opt, arg_u64, resolve_within, Tool, ToolContext, ToolOutcome};
use async_trait::async_trait;
use ignore::WalkBuilder;
use nash_core::error::{EngineError, Result};
use regex::Regex;
use serde_json::json;
use std::io::BufRead;

/// Search file contents with a regular expression.
pub struct SearchText;

#[async_trait]
impl Tool for SearchText {
    fn name(&self) -> &str {
        "search_text"
    }

    fn description(&self) -> &str {
        "Search workspace file contents with a regular expression, honouring .gitignore. Returns matching lines as path:line:text."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Rust regular expression" },
                "path": { "type": "string", "description": "Directory to search (default '.')" },
                "glob": { "type": "string", "description": "Only search files matching this glob, e.g. '*.rs'" },
                "max_results": { "type": "integer", "description": "Maximum matches (default 100)" }
            },
            "required": ["pattern"]
        })
    }

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let pattern = arg_str(&args, "pattern")?;
        let re = Regex::new(&pattern)
            .map_err(|e| EngineError::Tool(format!("invalid regex: {e}")))?;
        let rel = arg_str_opt(&args, "path").unwrap_or_else(|| ".".to_string());
        let root = resolve_within(&ctx.working_dir, &rel)?;
        let glob = arg_str_opt(&args, "glob");
        let glob_matcher = match &glob {
            Some(g) => Some(
                glob::Pattern::new(g)
                    .map_err(|e| EngineError::Tool(format!("invalid glob: {e}")))?,
            ),
            None => None,
        };
        let max = arg_u64(&args, "max_results").unwrap_or(100).max(1) as usize;

        let mut hits: Vec<String> = Vec::new();
        for entry in WalkBuilder::new(&root).build() {
            if ctx.cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            if hits.len() >= max {
                break;
            }
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let file_path = entry.path();
            if let Some(m) = &glob_matcher {
                let name = file_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                if !m.matches(&name) {
                    continue;
                }
            }
            if let Ok(file) = std::fs::File::open(file_path) {
                let reader = std::io::BufReader::new(file);
                for (idx, line) in reader.lines().take(20_000).enumerate() {
                    let line = match line {
                        Ok(l) => l,
                        Err(_) => break,
                    };
                    if re.is_match(&line) {
                        let display = file_path
                            .strip_prefix(&ctx.working_dir)
                            .unwrap_or(file_path)
                            .display();
                        hits.push(format!("{display}:{}:{}", idx + 1, line.trim_end()));
                        if hits.len() >= max {
                            break;
                        }
                    }
                }
            }
        }
        if hits.is_empty() {
            return Ok(ToolOutcome::ok("no matches"));
        }
        let (content, truncated) = ctx.truncate(hits.join("\n"));
        Ok(ToolOutcome {
            content,
            is_error: false,
            truncated,
        })
    }
}

/// Find files by glob pattern.
pub struct GlobFiles;

#[async_trait]
impl Tool for GlobFiles {
    fn name(&self) -> &str {
        "glob_files"
    }

    fn description(&self) -> &str {
        "List workspace files matching a glob pattern, e.g. 'src/**/*.rs'."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern relative to the workspace root" },
                "max_results": { "type": "integer", "description": "Maximum paths (default 200)" }
            },
            "required": ["pattern"]
        })
    }

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let pattern = arg_str(&args, "pattern")?;
        let max = arg_u64(&args, "max_results").unwrap_or(200).max(1) as usize;
        let full = ctx.working_dir.join(pattern);
        let full = full.to_string_lossy().to_string();
        let mut paths = Vec::new();
        let entries =
            glob::glob(&full).map_err(|e| EngineError::Tool(format!("invalid glob: {e}")))?;
        for entry in entries.flatten() {
            if ctx.cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let display = entry
                .strip_prefix(&ctx.working_dir)
                .unwrap_or(&entry)
                .display()
                .to_string();
            paths.push(display);
            if paths.len() >= max {
                break;
            }
        }
        if paths.is_empty() {
            return Ok(ToolOutcome::ok("no files matched"));
        }
        paths.sort();
        let (content, truncated) = ctx.truncate(paths.join("\n"));
        Ok(ToolOutcome {
            content,
            is_error: false,
            truncated,
        })
    }
}
