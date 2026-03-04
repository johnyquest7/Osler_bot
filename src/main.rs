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
                WorkerRequest::Chat { system, history, tools } => {
                    // Clone secrets before the await so no guard is held across it
                    let secrets_snap = secrets_watch_rx.borrow().clone();
                    let cfg = config_for_worker.ai.clone();

                    let result = ai_client
                        .send(&cfg, &secrets_snap, &system, &history, &tools)
                        .await;

                    match result {
                        Ok(crate::ai::AiResponse::Text(t)) => {
                            let _ = worker_response_tx.send(WorkerResponse::Text(t));
                        }
                        Ok(crate::ai::AiResponse::ToolCall { name, params }) => {
                            let _ = worker_response_tx.send(WorkerResponse::ToolCall {
                                name: name.clone(),
                                params: params.clone(),
                            });
                            match tool_registry_worker.execute(&name, params).await {
                                Ok(s) => {
                                    let _ = worker_response_tx.send(WorkerResponse::Text(s));
                                }
                                Err(e) => {
                                    let _ = worker_response_tx
                                        .send(WorkerResponse::Error(e.to_string()));
                                }
                            }
                        }
                        Err(e) => {
                            let _ = worker_response_tx
                                .send(WorkerResponse::Error(e.to_string()));
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

                        let result = ai_client
                            .send(
                                &config_tg.ai,
                                &worker_secrets_tg,
                                &system,
                                &tg_mem.messages(),
                                &tools_tg.definitions(),
                            )
                            .await;

                        let reply = match result {
                            Ok(crate::ai::AiResponse::Text(t)) => {
                                tg_mem.push(memory::short_term::ChatMessage::assistant(t.clone()));
                                t
                            }
                            Ok(crate::ai::AiResponse::ToolCall { name, params }) => {
                                match tools_tg.execute(&name, params).await {
                                    Ok(out) => {
                                        tg_mem.push(
                                            memory::short_term::ChatMessage::tool_result(&name, &out),
                                        );
                                        format!("Tool `{name}`:\n{out}")
                                    }
                                    Err(e) => format!("Tool error: {e}"),
                                }
                            }
                            Err(e) => format!("Error: {e}"),
                        };

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
