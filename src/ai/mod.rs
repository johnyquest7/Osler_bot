/// AI provider abstraction – OpenAI, Anthropic (Claude), Google Gemini.
///
/// Keys are placed in HTTP headers only and never serialised into history.
///
/// Tool-calling loop is the caller's responsibility (see main.rs worker).
/// Primitives provided:
///   - `history_to_messages`   ChatMessage → provider-specific Value
///   - `send`                  one API round-trip, may return ToolCall
///   - `append_tool_result`    add assistant tool_call + result to messages

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde_json::{json, Value};

use crate::config::{AiConfig, AiProvider};
use crate::config::secrets::WorkerSecrets;
use crate::memory::short_term::{ChatMessage, Role};
use crate::tools::ToolDef;

// ─── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct ToolCallInfo {
    /// Provider-assigned call ID (links tool_result back to tool_call)
    pub call_id: String,
    pub name: String,
    pub params: Value,
    /// Raw provider-specific assistant message to re-append before tool result
    pub raw_assistant_msg: Value,
}

#[derive(Debug)]
pub enum AiResponse {
    Text(String),
    ToolCall(ToolCallInfo),
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

    /// Convert ChatMessage history to provider-specific JSON messages.
    /// System prompt is NOT included here – injected inside send().
    /// Tool messages from previous turns are skipped (already processed by AI).
    pub fn history_to_messages(provider: &AiProvider, history: &[ChatMessage]) -> Vec<Value> {
        match provider {
            AiProvider::OpenAI | AiProvider::Anthropic => history
                .iter()
                .filter_map(|m| match m.role {
                    Role::User => Some(json!({"role": "user", "content": m.content})),
                    Role::Assistant => Some(json!({"role": "assistant", "content": m.content})),
                    _ => None,
                })
                .collect(),
            AiProvider::Gemini => history
                .iter()
                .filter_map(|m| match m.role {
                    Role::User => {
                        Some(json!({"role": "user", "parts": [{"text": m.content}]}))
                    }
                    Role::Assistant => {
                        Some(json!({"role": "model", "parts": [{"text": m.content}]}))
                    }
                    _ => None,
                })
                .collect(),
        }
    }

    /// Append the assistant's tool_call message + the tool result to messages.
    /// Call this before the next send() to continue the conversation.
    pub fn append_tool_result(
        provider: &AiProvider,
        messages: &mut Vec<Value>,
        call: &ToolCallInfo,
        result: &str,
    ) {
        messages.push(call.raw_assistant_msg.clone());
        match provider {
            AiProvider::OpenAI => {
                messages.push(json!({
                    "role": "tool",
                    "content": result,
                    "tool_call_id": call.call_id
                }));
            }
            AiProvider::Anthropic => {
                messages.push(json!({
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": call.call_id,
                        "content": result
                    }]
                }));
            }
            AiProvider::Gemini => {
                messages.push(json!({
                    "role": "user",
                    "parts": [{"functionResponse": {
                        "name": call.name,
                        "response": {"result": result}
                    }}]
                }));
            }
        }
    }

    /// One API round-trip.  `messages` must already be in provider format.
    pub async fn send(
        &self,
        config: &AiConfig,
        secrets: &WorkerSecrets,
        system: &str,
        messages: &[Value],
        tools: &[ToolDef],
    ) -> Result<AiResponse> {
        match config.provider {
            AiProvider::OpenAI => {
                let key = secrets.openai_api_key.as_deref()
                    .ok_or_else(|| anyhow::anyhow!("OpenAI API key not configured"))?;
                self.openai_send(key, &config.model, system, messages, tools).await
            }
            AiProvider::Anthropic => {
                let key = secrets.anthropic_api_key.as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Anthropic API key not configured"))?;
                self.anthropic_send(key, &config.model, system, messages, tools).await
            }
            AiProvider::Gemini => {
                let key = secrets.gemini_api_key.as_deref()
                    .ok_or_else(|| anyhow::anyhow!("Gemini API key not configured"))?;
                self.gemini_send(key, &config.model, system, messages, tools).await
            }
        }
    }

    // ── OpenAI ───────────────────────────────────────────────────────────────

    async fn openai_send(
        &self,
        api_key: &str,
        model: &str,
        system: &str,
        messages: &[Value],
        tools: &[ToolDef],
    ) -> Result<AiResponse> {
        let mut full = vec![json!({"role": "system", "content": system})];
        full.extend_from_slice(messages);

        let mut body = json!({"model": model, "messages": full});

        if !tools.is_empty() {
            let defs: Vec<Value> = tools.iter().map(|t| json!({
                "type": "function",
                "function": {
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters_schema,
                }
            })).collect();
            body["tools"] = json!(defs);
            body["tool_choice"] = json!("auto");
        }

        let resp = self.http
            .post("https://api.openai.com/v1/chat/completions")
            .bearer_auth(api_key)
            .json(&body)
            .send().await
            .context("OpenAI HTTP request")?;

        let status = resp.status();
        let text = resp.text().await.context("Reading OpenAI response")?;
        if !status.is_success() { bail!("OpenAI {status}: {text}"); }

        let j: Value = serde_json::from_str(&text).context("Parsing OpenAI JSON")?;
        parse_openai(&j)
    }

    // ── Anthropic ─────────────────────────────────────────────────────────────

    async fn anthropic_send(
        &self,
        api_key: &str,
        model: &str,
        system: &str,
        messages: &[Value],
        tools: &[ToolDef],
    ) -> Result<AiResponse> {
        let mut body = json!({
            "model": model,
            "max_tokens": 4096,
            "system": system,
            "messages": messages,
        });

        if !tools.is_empty() {
            let defs: Vec<Value> = tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.parameters_schema,
            })).collect();
            body["tools"] = json!(defs);
        }

        let resp = self.http
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send().await
            .context("Anthropic HTTP request")?;

        let status = resp.status();
        let text = resp.text().await.context("Reading Anthropic response")?;
        if !status.is_success() { bail!("Anthropic {status}: {text}"); }

        let j: Value = serde_json::from_str(&text).context("Parsing Anthropic JSON")?;
        parse_anthropic(&j)
    }

    // ── Gemini ────────────────────────────────────────────────────────────────

    async fn gemini_send(
        &self,
        api_key: &str,
        model: &str,
        system: &str,
        messages: &[Value],
        tools: &[ToolDef],
    ) -> Result<AiResponse> {
        let mut body = json!({
            "system_instruction": {"parts": [{"text": system}]},
            "contents": messages,
            "generationConfig": {"maxOutputTokens": 4096}
        });

        if !tools.is_empty() {
            let decls: Vec<Value> = tools.iter().map(|t| json!({
                "name": t.name,
                "description": t.description,
                "parameters": t.parameters_schema,
            })).collect();
            body["tools"] = json!([{"function_declarations": decls}]);
        }

        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{}:generateContent?key={}",
            model, api_key
        );

        let resp = self.http.post(&url).json(&body).send().await
            .context("Gemini HTTP request")?;

        let status = resp.status();
        let text = resp.text().await.context("Reading Gemini response")?;
        if !status.is_success() { bail!("Gemini {status}: {text}"); }

        let j: Value = serde_json::from_str(&text).context("Parsing Gemini JSON")?;
        parse_gemini(&j)
    }
}

// ─── Response parsers ──────────────────────────────────────────────────────────

fn parse_openai(j: &Value) -> Result<AiResponse> {
    let msg = &j["choices"][0]["message"];

    if let Some(tcs) = msg["tool_calls"].as_array() {
        if let Some(tc) = tcs.first() {
            let call_id = tc["id"].as_str().unwrap_or("call_0").to_string();
            let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
            let params: Value = serde_json::from_str(
                tc["function"]["arguments"].as_str().unwrap_or("{}"),
            ).unwrap_or(json!({}));

            let raw_assistant_msg = json!({
                "role": "assistant",
                "content": null,
                "tool_calls": msg["tool_calls"]
            });

            return Ok(AiResponse::ToolCall(ToolCallInfo {
                call_id, name, params, raw_assistant_msg,
            }));
        }
    }

    let text = msg["content"].as_str()
        .ok_or_else(|| anyhow::anyhow!("No content in OpenAI response:\n{j}"))?
        .to_string();
    Ok(AiResponse::Text(text))
}

fn parse_anthropic(j: &Value) -> Result<AiResponse> {
    if let Some(blocks) = j["content"].as_array() {
        for b in blocks {
            match b["type"].as_str() {
                Some("tool_use") => {
                    return Ok(AiResponse::ToolCall(ToolCallInfo {
                        call_id: b["id"].as_str().unwrap_or("tu_0").to_string(),
                        name: b["name"].as_str().unwrap_or("").to_string(),
                        params: b["input"].clone(),
                        raw_assistant_msg: json!({
                            "role": "assistant",
                            "content": j["content"]
                        }),
                    }));
                }
                Some("text") => {
                    return Ok(AiResponse::Text(
                        b["text"].as_str().unwrap_or("").to_string(),
                    ));
                }
                _ => {}
            }
        }
    }
    bail!("Unexpected Anthropic response:\n{j}")
}

fn parse_gemini(j: &Value) -> Result<AiResponse> {
    if let Some(parts) = j["candidates"][0]["content"]["parts"].as_array() {
        for p in parts {
            if let Some(fc) = p.get("functionCall") {
                let name = fc["name"].as_str().unwrap_or("").to_string();
                return Ok(AiResponse::ToolCall(ToolCallInfo {
                    call_id: name.clone(),
                    name,
                    params: fc["args"].clone(),
                    raw_assistant_msg: json!({"role": "model", "parts": [{"functionCall": fc}]}),
                }));
            }
            if let Some(text) = p["text"].as_str() {
                return Ok(AiResponse::Text(text.to_string()));
            }
        }
    }
    bail!("Unexpected Gemini response:\n{j}")
}
