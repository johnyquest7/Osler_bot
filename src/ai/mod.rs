/// AI provider abstraction.
///
/// Supports OpenAI, Anthropic (Claude), and Google Gemini through direct HTTP
/// requests.  Keys are only accessed inside the `send` function and are passed
/// as HTTP headers – they are never serialised into the message history or
/// included in any log output.

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::config::{AiConfig, AiProvider};
use crate::config::secrets::WorkerSecrets;
use crate::memory::short_term::ChatMessage;
use crate::tools::ToolDef;

// ─── Public request / response types ─────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum AiResponse {
    /// Normal text response
    Text(String),
    /// The model wants to call a tool
    ToolCall { name: String, params: Value },
}

// ─── AiClient ────────────────────────────────────────────────────────────────

pub struct AiClient {
    http: Client,
}

impl AiClient {
    pub fn new() -> Result<Self> {
        let http = Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()
            .context("Building HTTP client")?;
        Ok(Self { http })
    }

    /// Send a chat request and return the assistant's response.
    ///
    /// `system` is the full system prompt (already built by `persona`).
    /// `history` is the current short-term message window.
    /// `tools` is the list of registered tool definitions (may be empty).
    pub async fn send(
        &self,
        config: &AiConfig,
        secrets: &WorkerSecrets,
        system: &str,
        history: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<AiResponse> {
        match config.provider {
            AiProvider::OpenAI => {
                let key = secrets
                    .openai_api_key
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("OpenAI API key not configured"))?;
                self.openai_chat(key, &config.model, system, history, tools).await
            }
            AiProvider::Anthropic => {
                let key = secrets
                    .anthropic_api_key
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Anthropic API key not configured"))?;
                self.anthropic_chat(key, &config.model, system, history, tools).await
            }
            AiProvider::Gemini => {
                let key = secrets
                    .gemini_api_key
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Gemini API key not configured"))?;
                self.gemini_chat(key, &config.model, system, history, tools).await
            }
        }
    }

    // ── OpenAI ───────────────────────────────────────────────────────────────

    async fn openai_chat(
        &self,
        api_key: &str,
        model: &str,
        system: &str,
        history: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<AiResponse> {
        let mut messages = vec![json!({"role": "system", "content": system})];

        for msg in history {
            let role = msg.role.as_str();
            if msg.role == crate::memory::short_term::Role::Tool {
                messages.push(json!({
                    "role": "tool",
                    "content": msg.content,
                    "tool_call_id": msg.tool_name.as_deref().unwrap_or("unknown")
                }));
            } else {
                messages.push(json!({"role": role, "content": msg.content}));
            }
        }

        let mut body = json!({
            "model": model,
            "messages": messages,
        });

        if !tools.is_empty() {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = json!(tool_defs);
            body["tool_choice"] = json!("auto");
        }

        let resp = self
            .http
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(api_key)
            .json(&body)
            .send()
            .await
            .context("OpenAI request")?;

        let status = resp.status();
        let text = resp.text().await.context("Reading OpenAI response")?;

        if !status.is_success() {
            bail!("OpenAI API error {status}: {text}");
        }

        let json: Value = serde_json::from_str(&text).context("Parsing OpenAI response")?;
        parse_openai_response(&json)
    }

    // ── Anthropic ─────────────────────────────────────────────────────────────

    async fn anthropic_chat(
        &self,
        api_key: &str,
        model: &str,
        system: &str,
        history: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<AiResponse> {
        use crate::memory::short_term::Role;

        let messages: Vec<Value> = history
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| {
                let role = match m.role {
                    Role::User => "user",
                    Role::Assistant => "assistant",
                    Role::Tool => "user", // Anthropic uses user turn for tool results
                    Role::System => unreachable!(),
                };
                json!({"role": role, "content": m.content})
            })
            .collect();

        let mut body = json!({
            "model": model,
            "max_tokens": 4096,
            "system": system,
            "messages": messages,
        });

        if !tools.is_empty() {
            let tool_defs: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters_schema,
                    })
                })
                .collect();
            body["tools"] = json!(tool_defs);
        }

        let resp = self
            .http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Anthropic request")?;

        let status = resp.status();
        let text = resp.text().await.context("Reading Anthropic response")?;

        if !status.is_success() {
            bail!("Anthropic API error {status}: {text}");
        }

        let json: Value = serde_json::from_str(&text).context("Parsing Anthropic response")?;
        parse_anthropic_response(&json)
    }

    // ── Gemini ────────────────────────────────────────────────────────────────

    async fn gemini_chat(
        &self,
        api_key: &str,
        model: &str,
        system: &str,
        history: &[ChatMessage],
        tools: &[ToolDef],
    ) -> Result<AiResponse> {
        use crate::memory::short_term::Role;

        let contents: Vec<Value> = history
            .iter()
            .filter(|m| m.role != Role::System)
            .map(|m| {
                let role = match m.role {
                    Role::User | Role::Tool => "user",
                    Role::Assistant => "model",
                    Role::System => unreachable!(),
                };
                json!({
                    "role": role,
                    "parts": [{"text": m.content}]
                })
            })
            .collect();

        let mut body = json!({
            "system_instruction": {"parts": [{"text": system}]},
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": 4096,
            }
        });

        if !tools.is_empty() {
            let fn_decls: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters_schema,
                    })
                })
                .collect();
            body["tools"] = json!([{"function_declarations": fn_decls}]);
        }

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model, api_key
        );

        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .context("Gemini request")?;

        let status = resp.status();
        let text = resp.text().await.context("Reading Gemini response")?;

        if !status.is_success() {
            bail!("Gemini API error {status}: {text}");
        }

        let json: Value = serde_json::from_str(&text).context("Parsing Gemini response")?;
        parse_gemini_response(&json)
    }
}

// ─── Response parsers ─────────────────────────────────────────────────────────

fn parse_openai_response(json: &Value) -> Result<AiResponse> {
    let choice = &json["choices"][0];
    let msg = &choice["message"];

    // Check for tool call
    if let Some(tool_calls) = msg["tool_calls"].as_array() {
        if let Some(tc) = tool_calls.first() {
            let name = tc["function"]["name"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let params: Value = serde_json::from_str(
                tc["function"]["arguments"].as_str().unwrap_or("{}"),
            )
            .unwrap_or(json!({}));
            return Ok(AiResponse::ToolCall { name, params });
        }
    }

    let text = msg["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No content in OpenAI response: {json}"))?
        .to_string();
    Ok(AiResponse::Text(text))
}

fn parse_anthropic_response(json: &Value) -> Result<AiResponse> {
    if let Some(content) = json["content"].as_array() {
        for block in content {
            match block["type"].as_str() {
                Some("tool_use") => {
                    let name = block["name"].as_str().unwrap_or("").to_string();
                    let params = block["input"].clone();
                    return Ok(AiResponse::ToolCall { name, params });
                }
                Some("text") => {
                    let text = block["text"].as_str().unwrap_or("").to_string();
                    return Ok(AiResponse::Text(text));
                }
                _ => {}
            }
        }
    }
    bail!("Unexpected Anthropic response structure: {json}")
}

fn parse_gemini_response(json: &Value) -> Result<AiResponse> {
    let candidate = &json["candidates"][0];
    let parts = candidate["content"]["parts"].as_array();

    if let Some(parts) = parts {
        for part in parts {
            if let Some(fc) = part.get("functionCall") {
                let name = fc["name"].as_str().unwrap_or("").to_string();
                let params = fc["args"].clone();
                return Ok(AiResponse::ToolCall { name, params });
            }
            if let Some(text) = part["text"].as_str() {
                return Ok(AiResponse::Text(text.to_string()));
            }
        }
    }
    bail!("Unexpected Gemini response structure: {json}")
}
