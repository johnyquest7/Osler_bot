mod ai;
mod app;
mod config;
mod memory;
mod persona;
mod telegram;
mod tools;
mod ui;

use std::sync::{mpsc, Arc};

use anyhow::Result;
use tokio::sync::{mpsc as async_mpsc, watch};
use tracing_subscriber::EnvFilter;

use crate::ai::AiClient;
use crate::app::{OslerApp, WorkerRequest, WorkerResponse};
use crate::config::secrets::{SecretStore, WorkerSecrets};
use crate::config::AppConfig;
use crate::tools::ToolRegistry;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = AppConfig::load().unwrap_or_else(|e| {
        tracing::warn!("Config load failed ({e}), using defaults");
        AppConfig::default()
    });

    let secret_store = SecretStore::new();
    let secrets = secret_store.load().unwrap_or_else(|e| {
        tracing::warn!("Secrets load failed ({e}), starting empty");
        config::secrets::Secrets::default()
    });

    // Build the Send+Clone secrets snapshot for async tasks
    let worker_secrets_init = WorkerSecrets::from_secrets(&secrets);
    let tg_token = worker_secrets_init.telegram_bot_token.clone();
    let search_key = worker_secrets_init.search_api_key.clone();

    // ── Tokio runtime ──────────────────────────────────────────────────────
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    // ── Async request channel (GUI → worker, truly async recv) ────────────
    let (worker_tx, mut worker_request_rx) =
        tokio::sync::mpsc::unbounded_channel::<WorkerRequest>();

    // ── Sync response channel (worker → GUI, polled with try_recv) ────────
    let (worker_response_tx, worker_rx) = mpsc::channel::<WorkerResponse>();

    // ── Secrets watch (GUI pushes updates when user saves settings) ────────
    let (secrets_watch_tx, secrets_watch_rx) = watch::channel(worker_secrets_init.clone());

    // ── HTTP client & tools ───────────────────────────────────────────────
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;

    let tool_registry = Arc::new(ToolRegistry::with_defaults(http.clone(), search_key));
    let tool_registry_worker = Arc::clone(&tool_registry);

    let config_for_worker = config.clone();

    // ── Async AI worker ────────────────────────────────────────────────────
    rt.spawn(async move {
        let ai_client = match AiClient::new() {
            Ok(c) => c,
            Err(e) => { tracing::error!("AI client init failed: {e}"); return; }
        };

        loop {
            let req = match worker_request_rx.recv().await {
                Some(r) => r,
                None => break,
            };

            match req {
                WorkerRequest::Shutdown => break,
                WorkerRequest::Chat { system, mut messages, tools, provider } => {
                    // Clone secrets before the await so no guard is held across it
                    let secrets_snap = secrets_watch_rx.borrow().clone();
                    let cfg = config_for_worker.ai.clone();

                    // Multi-turn tool calling loop (max 8 iterations to avoid runaway)
                    let mut iterations = 0u32;
                    loop {
                        iterations += 1;
                        if iterations > 8 {
                            let _ = worker_response_tx
                                .send(WorkerResponse::Error("Too many tool iterations".into()));
                            break;
                        }

                        let result = ai_client
                            .send(&cfg, &secrets_snap, &system, &messages, &tools)
                            .await;

                        match result {
                            Ok(crate::ai::AiResponse::Text(t)) => {
                                let _ = worker_response_tx.send(WorkerResponse::Text(t));
                                break;
                            }
                            Ok(crate::ai::AiResponse::ToolCall(call)) => {
                                let _ = worker_response_tx.send(WorkerResponse::ToolStatus(
                                    format!("Running tool: {}…", call.name),
                                ));

                                let tool_output = match tool_registry_worker
                                    .execute(&call.name, call.params.clone())
                                    .await
                                {
                                    Ok(s) => s,
                                    Err(e) => format!("Tool error: {e}"),
                                };

                                let _ = worker_response_tx.send(WorkerResponse::ToolResult {
                                    name: call.name.clone(),
                                    output: tool_output.clone(),
                                });

                                // Append assistant tool_call + result, then loop back to AI
                                AiClient::append_tool_result(
                                    &provider, &mut messages, &call, &tool_output,
                                );
                            }
                            Err(e) => {
                                let _ = worker_response_tx
                                    .send(WorkerResponse::Error(e.to_string()));
                                break;
                            }
                        }
                    }
                }
            }
        }
    });

    // ── Telegram bot (optional) ────────────────────────────────────────────
    if config.telegram.enabled {
        if let Some(allowed_id) = config.telegram.allowed_user_id {
            if let Some(token) = tg_token {
                let (tg_req_tx, mut tg_req_rx) =
                    async_mpsc::channel::<telegram::TelegramAiRequest>(32);
                let (tg_resp_tx, tg_resp_rx) =
                    async_mpsc::channel::<telegram::TelegramAiResponse>(32);

                let config_tg = config.clone();
                let tools_tg = Arc::clone(&tool_registry);
                let worker_secrets_tg = worker_secrets_init.clone();

                rt.spawn(async move {
                    let ai_client = match AiClient::new() {
                        Ok(c) => c,
                        Err(e) => { tracing::error!("Telegram AI client failed: {e}"); return; }
                    };
                    let mut tg_mem = memory::short_term::ShortTermMemory::new(
                        config_tg.memory.short_term_window,
                    );

                    while let Some(req) = tg_req_rx.recv().await {
                        tg_mem.push(memory::short_term::ChatMessage::user(req.text.clone()));

                        let tool_desc = tools_tg.description_block();
                        let system = persona::SystemPrompt::build(
                            &config_tg.ai, &config_tg.user, "", &tool_desc,
                        ).content;

                        let provider = config_tg.ai.provider.clone();
                        let mut messages = AiClient::history_to_messages(&provider, &tg_mem.messages());
                        let tools = tools_tg.definitions();

                        // Multi-turn tool loop for Telegram
                        let mut reply = String::from("Error: no response");
                        let mut iters = 0u32;
                        loop {
                            iters += 1;
                            if iters > 8 { reply = "Too many tool iterations".into(); break; }

                            match ai_client.send(&config_tg.ai, &worker_secrets_tg, &system, &messages, &tools).await {
                                Ok(crate::ai::AiResponse::Text(t)) => {
                                    tg_mem.push(memory::short_term::ChatMessage::assistant(t.clone()));
                                    reply = t;
                                    break;
                                }
                                Ok(crate::ai::AiResponse::ToolCall(call)) => {
                                    let out = match tools_tg.execute(&call.name, call.params.clone()).await {
                                        Ok(s) => s,
                                        Err(e) => format!("Tool error: {e}"),
                                    };
                                    tg_mem.push(memory::short_term::ChatMessage::tool_result(&call.name, &out));
                                    AiClient::append_tool_result(&provider, &mut messages, &call, &out);
                                }
                                Err(e) => { reply = format!("Error: {e}"); break; }
                            }
                        }

                        let _ = tg_resp_tx
                            .send(telegram::TelegramAiResponse { chat_id: req.chat_id, text: reply })
                            .await;
                    }
                });

                rt.spawn(telegram::run_bot(token, allowed_id, tg_req_tx, tg_resp_rx));
                tracing::info!("Telegram bot started");
            } else {
                tracing::warn!("Telegram enabled but no token configured");
            }
        } else {
            tracing::warn!("Telegram enabled but no allowed_user_id configured");
        }
    }

    // ── egui / eframe ──────────────────────────────────────────────────────
    let native_opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Osler")
            .with_inner_size([900.0, 650.0])
            .with_min_inner_size([600.0, 400.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Osler",
        native_opts,
        Box::new(move |_cc| {
            Ok(Box::new(OslerApp::new(
                config,
                secrets,
                secret_store,
                worker_tx,
                worker_rx,
                tool_registry,
                secrets_watch_tx,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe error: {e}"))
}
