/// Tool: run a shell command and return stdout/stderr.
///
/// Security notes:
///   - Commands run as the current user – no privilege escalation.
///   - The AI is instructed (in the system prompt) to confirm destructive
///     commands before executing them.
///   - Output is capped at 8 KB to avoid context flooding.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use tokio::process::Command;

use super::{Tool, ToolDef};

const MAX_OUTPUT: usize = 8 * 1024; // 8 KiB

pub struct ShellTool;

#[async_trait]
impl Tool for ShellTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "run_shell".into(),
            description: "Execute a shell command and return its stdout and stderr. \
                          Use this for file operations, running scripts, querying system \
                          state, etc. Always ask the user before running destructive commands."
                .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "The shell command to run (executed via /bin/sh -c)"
                    },
                    "working_dir": {
                        "type": "string",
                        "description": "Optional working directory. Defaults to the user's home dir."
                    }
                },
                "required": ["command"]
            }),
        }
    }

    async fn execute(&self, params: Value) -> Result<String> {
        let command = params["command"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;

        let working_dir = params["working_dir"]
            .as_str()
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| {
                dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."))
            });

        tracing::info!(command = command, "ShellTool executing");

        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(command)
            .current_dir(&working_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await?;

        let mut result = String::new();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        if !stdout.is_empty() {
            result.push_str("STDOUT:\n");
            result.push_str(truncate(&stdout, MAX_OUTPUT / 2));
            result.push('\n');
        }
        if !stderr.is_empty() {
            result.push_str("STDERR:\n");
            result.push_str(truncate(&stderr, MAX_OUTPUT / 2));
            result.push('\n');
        }

        result.push_str(&format!("EXIT CODE: {}", output.status.code().unwrap_or(-1)));
        Ok(result)
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        &s[..max]
    }
}
