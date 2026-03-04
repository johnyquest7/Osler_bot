pub mod filesystem;
pub mod shell;
pub mod web_fetch;
pub mod web_search;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

/// Describes a tool for the AI function-calling API.
#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema object for the tool's input parameters.
    pub parameters_schema: Value,
}

/// A tool the AI model can invoke.
#[async_trait]
pub trait Tool: Send + Sync {
    fn def(&self) -> ToolDef;
    async fn execute(&self, params: Value) -> Result<String>;
}

/// Registry of all enabled tools.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self { tools: Vec::new() }
    }

    /// Register all default tools.
    pub fn with_defaults(http: reqwest::Client, search_api_key: Option<String>) -> Self {
        let mut reg = Self::new();
        reg.register(Box::new(shell::ShellTool));
        reg.register(Box::new(filesystem::FileSystemTool));
        reg.register(Box::new(web_fetch::WebFetchTool { http: http.clone() }));
        if let Some(key) = search_api_key {
            reg.register(Box::new(web_search::WebSearchTool {
                http,
                api_key: key,
            }));
        }
        reg
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.push(tool);
    }

    /// Returns the `ToolDef` list to send to the AI provider.
    pub fn definitions(&self) -> Vec<ToolDef> {
        self.tools.iter().map(|t| t.def()).collect()
    }

    /// Returns a human-readable description block for the system prompt.
    pub fn description_block(&self) -> String {
        self.tools
            .iter()
            .map(|t| {
                let d = t.def();
                format!("- **{}**: {}", d.name, d.description)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Find and execute a tool by name.
    pub async fn execute(&self, name: &str, params: Value) -> Result<String> {
        for tool in &self.tools {
            if tool.def().name == name {
                return tool.execute(params).await;
            }
        }
        Err(anyhow::anyhow!("Unknown tool: {name}"))
    }
}
