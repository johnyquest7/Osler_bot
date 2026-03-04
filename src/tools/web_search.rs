/// Tool: web search using the Brave Search API.
///
/// Requires a Brave Search API key (free tier available).
/// Falls back gracefully when no key is configured (tool simply won't be
/// registered in the ToolRegistry).

use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

use super::{Tool, ToolDef};

pub struct WebSearchTool {
    pub http: Client,
    pub api_key: String,
}

#[async_trait]
impl Tool for WebSearchTool {
    fn def(&self) -> ToolDef {
        ToolDef {
            name: "web_search".into(),
            description: "Search the web using Brave Search. Returns titles, URLs, and snippets \
                          for the top results. Use this to find current information."
                .into(),
            parameters_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The search query"
                    },
                    "count": {
                        "type": "integer",
                        "description": "Number of results to return (1-10, default 5)",
                        "minimum": 1,
                        "maximum": 10
                    }
                },
                "required": ["query"]
            }),
        }
    }

    async fn execute(&self, params: Value) -> Result<String> {
        let query = params["query"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing 'query'"))?;
        let count = params["count"].as_u64().unwrap_or(5).min(10) as u32;

        let resp = self
            .http
            .get("https://api.search.brave.com/res/v1/web/search")
            .header("Accept", "application/json")
            .header("Accept-Encoding", "gzip")
            .header("X-Subscription-Token", &self.api_key)
            .query(&[("q", query), ("count", &count.to_string())])
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await?;
            return Err(anyhow::anyhow!("Brave Search API error {status}: {body}"));
        }

        let json: Value = resp.json().await?;

        let mut output = Vec::new();
        if let Some(results) = json["web"]["results"].as_array() {
            for (i, r) in results.iter().enumerate().take(count as usize) {
                let title = r["title"].as_str().unwrap_or("(no title)");
                let url = r["url"].as_str().unwrap_or("");
                let snippet = r["description"].as_str().unwrap_or("");
                output.push(format!("{}. **{}**\n   {}\n   {}", i + 1, title, url, snippet));
            }
        }

        if output.is_empty() {
            Ok("No results found.".into())
        } else {
            Ok(output.join("\n\n"))
        }
    }
}
