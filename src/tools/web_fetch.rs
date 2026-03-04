/// Tool: fetch a URL and return its text content.

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use super::{Tool, ToolDef};

const MAX_BODY: usize = 16 * 1024; // 16 KiB

pub struct WebFetchTool {
    pub http: Client,
}

#[async_trait]
impl Tool for WebFetchTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "web_fetch".into(),
            description: "Fetch a URL and return its raw text content (HTML, JSON, plain text). \
                          Useful for reading documentation, APIs, or web pages."
                .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "url": {
                        "type": "string",
                        "description": "The URL to fetch"
                    }
                },
                "required": ["url"]
            }),
        }
    }

    async fn execute(&self, params: Value) -> Result<String> {
        let url = params["url"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'url'"))?;

        let resp = self
            .http
            .get(url)
            .header("User-Agent", "Osler-AI-Assistant/0.1")
            .send()
            .await?;

        let status = resp.status();
        let body = resp.bytes().await?;
        let text = String::from_utf8_lossy(&body[..body.len().min(MAX_BODY)]);

        Ok(format!("HTTP {status}\n\n{text}"))
    }
}
