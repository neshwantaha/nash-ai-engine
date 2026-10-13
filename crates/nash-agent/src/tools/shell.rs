//! Shell execution tool. High risk: gated by the approval policy by default.

use super::{arg_str, arg_u64, Risk, Tool, ToolContext, ToolOutcome};
use async_trait::async_trait;
use nash_core::error::{EngineError, Result};
use serde_json::json;

/// Run a shell command in the workspace.
pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn name(&self) -> &str {
        "run_command"
    }

    fn description(&self) -> &str {
        "Run a shell command in the workspace and return its combined output. Use sparingly; prefer the file and search tools."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Command line to run" },
                "timeout_ms": { "type": "integer", "description": "Timeout in milliseconds (default 60000)" }
            },
            "required": ["command"]
        })
    }

    fn risk(&self) -> Risk {
        Risk::High
    }

    async fn call(&self, args: serde_json::Value, ctx: &ToolContext) -> Result<ToolOutcome> {
        let command = arg_str(&args, "command")?;
        let timeout = std::time::Duration::from_millis(arg_u64(&args, "timeout_ms").unwrap_or(60_000));

        #[cfg(windows)]
        let mut cmd = {
            let mut c = tokio::process::Command::new("cmd");
            c.arg("/C").arg(&command);
            c
        };
        #[cfg(not(windows))]
        let mut cmd = {
            let mut c = tokio::process::Command::new("sh");
            c.arg("-c").arg(&command);
            c
        };

        let child = cmd
            .current_dir(&ctx.working_dir)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| EngineError::Tool(format!("cannot spawn command: {e}")))?;

        let output = tokio::select! {
            _ = ctx.cancel.cancelled() => return Err(EngineError::Cancelled),
            res = tokio::time::timeout(timeout, child.wait_with_output()) => {
                match res {
                    Ok(Ok(out)) => out,
                    Ok(Err(e)) => return Err(EngineError::Tool(format!("command failed: {e}"))),
                    Err(_) => return Ok(ToolOutcome::error(format!("command timed out after {} ms", timeout.as_millis()))),
                }
            }
        };

        let mut combined = String::new();
        combined.push_str(&String::from_utf8_lossy(&output.stdout));
        let err = String::from_utf8_lossy(&output.stderr);
        if !err.is_empty() {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str("[stderr]\n");
            combined.push_str(&err);
        }
        let status = output.status.code().unwrap_or(-1);
        let header = format!("exit code: {status}\n");
        let (body, truncated) = ctx.truncate(combined);
        Ok(ToolOutcome {
            content: format!("{header}{body}"),
            is_error: !output.status.success(),
            truncated,
        })
    }
}
