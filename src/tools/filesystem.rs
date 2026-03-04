/// Tool: read and write files on the local filesystem.
///
/// Paths that start with `/proc`, `/sys`, `/dev`, or that are the osler
/// secrets files are blocked to prevent accidental exposure.

use anyhow::{bail, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::Path;

use super::{Tool, ToolDef};
use crate::config::config_dir;

const MAX_READ: usize = 32 * 1024; // 32 KiB

pub struct FileSystemTool;

#[async_trait]
impl Tool for FileSystemTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "file_system".into(),
            description: "Read or write files. Operations: read_file, write_file, list_dir, \
                          delete_file. Paths must be absolute."
                .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["read_file", "write_file", "list_dir", "delete_file"],
                        "description": "The filesystem operation to perform"
                    },
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the file or directory"
                    },
                    "content": {
                        "type": "string",
                        "description": "Content to write (only for write_file)"
                    }
                },
                "required": ["operation", "path"]
            }),
        }
    }

    async fn execute(&self, params: Value) -> Result<String> {
        let operation = params["operation"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'operation'"))?;
        let path_str = params["path"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'path'"))?;

        let path = Path::new(path_str);
        guard_path(path)?;

        match operation {
            "read_file" => {
                let bytes = tokio::fs::read(path).await?;
                let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_READ)]);
                let truncated = bytes.len() > MAX_READ;
                Ok(if truncated {
                    format!("{}\n[... truncated at {MAX_READ} bytes ...]", text)
                } else {
                    text.into_owned()
                })
            }
            "write_file" => {
                let content = params["content"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("Missing 'content' for write_file"))?;
                if let Some(parent) = path.parent() {
                    tokio::fs::create_dir_all(parent).await?;
                }
                tokio::fs::write(path, content).await?;
                Ok(format!("Written {} bytes to {path_str}", content.len()))
            }
            "list_dir" => {
                let mut entries = tokio::fs::read_dir(path).await?;
                let mut lines = Vec::new();
                while let Some(entry) = entries.next_entry().await? {
                    let meta = entry.metadata().await?;
                    let kind = if meta.is_dir() { "DIR " } else { "FILE" };
                    lines.push(format!("{kind}  {}", entry.file_name().to_string_lossy()));
                }
                lines.sort();
                Ok(lines.join("\n"))
            }
            "delete_file" => {
                if path.is_dir() {
                    tokio::fs::remove_dir_all(path).await?;
                    Ok(format!("Deleted directory {path_str}"))
                } else {
                    tokio::fs::remove_file(path).await?;
                    Ok(format!("Deleted file {path_str}"))
                }
            }
            other => bail!("Unknown operation: {other}"),
        }
    }
}

/// Prevent access to sensitive or dangerous paths.
fn guard_path(path: &Path) -> Result<()> {
    let path_str = path.to_string_lossy();

    let blocked_prefixes = ["/proc", "/sys", "/dev"];
    for prefix in blocked_prefixes {
        if path_str.starts_with(prefix) {
            bail!("Access denied: {path_str} is a system path");
        }
    }

    // Block the osler secrets directory
    let secrets_dir = config_dir();
    if path.starts_with(&secrets_dir) {
        bail!("Access denied: the osler configuration directory is protected");
    }

    Ok(())
}
