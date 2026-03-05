/// Main application – owns all state and drives the egui event loop.
///
/// Async AI work runs on a background tokio runtime.  The GUI thread
/// communicates with the worker via:
///
///   worker_tx  (tokio UnboundedSender)  – GUI sends requests to worker
///   worker_rx  (std mpsc Receiver)      – worker sends responses, GUI polls
///   secrets_tx (tokio watch Sender)     – GUI pushes updated secrets to worker

use std::sync::mpsc;

use egui::{CentralPanel, Color32, Context, RichText, TopBottomPanel};
use tokio::sync::{mpsc::UnboundedSender, watch};

use crate::config::secrets::{SecretStore, Secrets, WorkerSecrets};
use crate::config::AppConfig;
use crate::memory::long_term::LongTermMemory;
use crate::memory::short_term::{ChatMessage, ShortTermMemory};
use crate::persona::SystemPrompt;
use crate::tools::{ToolDef, ToolRegistry};
use crate::ui::chat::ChatPanel;
use crate::ui::onboarding::Onboarding;
use crate::ui::settings::{SecretFields, SettingsPanel};
use std::sync::Arc;

// ─── Channel message types ────────────────────────────────────────────────────

pub enum WorkerRequest {
    Chat {
        system: String,
        /// Pre-built provider-format messages (built by AiClient::history_to_messages)
        messages: Vec<serde_json::Value>,
        tools: Vec<ToolDef>,
        provider: crate::config::AiProvider,
    },
    Shutdown,
}

pub enum WorkerResponse {
    Text(String),
    /// Shown in status bar while the tool is executing
    ToolStatus(String),
    /// Tool finished – show result bubble in chat
    ToolResult { name: String, output: String },
    Error(String),
}

// ─── Panel enum ───────────────────────────────────────────────────────────────

#[derive(PartialEq)]
enum Panel {
    Chat,
    Settings,
}

// ─── OslerApp ─────────────────────────────────────────────────────────────────

pub struct OslerApp {
    config: AppConfig,
    secrets: Secrets,
    secret_store: SecretStore,
    panel: Panel,
    chat_panel: ChatPanel,
    settings_fields: SecretFields,
    onboarding: Option<Onboarding>,
    short_term: ShortTermMemory,
    long_term: Option<LongTermMemory>,
    tool_registry: Arc<ToolRegistry>,
    worker_tx: UnboundedSender<WorkerRequest>,
    worker_rx: mpsc::Receiver<WorkerResponse>,
    secrets_tx: watch::Sender<WorkerSecrets>,
}

impl OslerApp {
    pub fn new(
        config: AppConfig,
        secrets: Secrets,
        secret_store: SecretStore,
        worker_tx: UnboundedSender<WorkerRequest>,
        worker_rx: mpsc::Receiver<WorkerResponse>,
        tool_registry: Arc<ToolRegistry>,
        secrets_tx: watch::Sender<WorkerSecrets>,
    ) -> Self {
        let short_term = ShortTermMemory::new(config.memory.short_term_window);
        let long_term = if config.memory.enable_long_term {
            match LongTermMemory::open() {
                Ok(m) => Some(m),
                Err(e) => { tracing::error!("Long-term memory: {e}"); None }
            }
        } else {
            None
        };
        let onboarding = if config.first_run { Some(Onboarding::new()) } else { None };
        let mut settings_fields = SecretFields::default();
        settings_fields.load_from(&secrets);
        Self {
            config, secrets, secret_store, panel: Panel::Chat,
            chat_panel: ChatPanel::new(), settings_fields, onboarding,
            short_term, long_term, tool_registry, worker_tx, worker_rx, secrets_tx,
        }
    }

    fn submit_message(&mut self, text: String) {
        self.short_term.push(ChatMessage::user(text.clone()));
        self.chat_panel.waiting = true;

        let memory_ctx = self.long_term.as_ref()
            .and_then(|lt| lt.search(&text, self.config.memory.long_term_results).ok())
            .map(|e| LongTermMemory::format_context(&e))
            .unwrap_or_default();

        let tool_desc = self.tool_registry.description_block();
        let system = SystemPrompt::build(
            &self.config.ai, &self.config.user, &memory_ctx, &tool_desc,
        ).content;

        let provider = self.config.ai.provider.clone();
        let messages = crate::ai::AiClient::history_to_messages(&provider, &self.short_term.messages());

        let _ = self.worker_tx.send(WorkerRequest::Chat {
            system,
            messages,
            tools: self.tool_registry.definitions(),
            provider,
        });
        self.chat_panel.status_line = "Waiting for AI…".into();
    }

    fn poll_worker(&mut self) {
        loop {
            match self.worker_rx.try_recv() {
                Ok(WorkerResponse::Text(text)) => {
                    self.chat_panel.waiting = false;
                    self.chat_panel.status_line.clear();
                    self.short_term.push(ChatMessage::assistant(text));
                    self.maybe_persist_memory();
                }
                Ok(WorkerResponse::ToolStatus(msg)) => {
                    // Keep spinner, just update status line
                    self.chat_panel.status_line = msg;
                }
                Ok(WorkerResponse::ToolResult { name, output }) => {
                    // Tool done – show bubble, keep spinner (AI still thinking)
                    self.short_term.push(ChatMessage::tool_result(&name, &output));
                }
                Ok(WorkerResponse::Error(err)) => {
                    self.chat_panel.waiting = false;
                    self.chat_panel.status_line = format!("Error: {err}");
                    self.short_term.push(ChatMessage::assistant(
                        format!("⚠ An error occurred: {err}"),
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.chat_panel.status_line =
                        "⚠ AI worker disconnected. Restart the app.".into();
                    break;
                }
            }
        }
    }

    fn maybe_persist_memory(&mut self) {
        if self.short_term.len() < self.config.memory.short_term_window { return; }
        if let Some(lt) = &self.long_term {
            let text = self.short_term.as_text();
            let summary = text.chars().take(120).collect::<String>();
            let _ = lt.store(&summary, &text, &[]);
        }
    }
}

impl eframe::App for OslerApp {
    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        ctx.set_visuals(egui::Visuals::dark());
        self.poll_worker();
        if self.chat_panel.waiting { ctx.request_repaint(); }

        // Onboarding overlay
        if let Some(ob) = &mut self.onboarding {
            let finished = ob.show(ctx, &mut self.config);
            if finished {
                self.onboarding = None;
                let _ = self.config.save();
            }
            return;
        }

        TopBottomPanel::top("topbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new(&self.config.ai.ai_name)
                    .color(Color32::from_rgb(100, 180, 255)));
                ui.separator();
                ui.selectable_value(&mut self.panel, Panel::Chat, "💬 Chat");
                ui.selectable_value(&mut self.panel, Panel::Settings, "⚙ Settings");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(self.secret_store.backend_label())
                        .small().color(Color32::GRAY));
                });
            });
        });

        CentralPanel::default().show(ctx, |ui| {
            match self.panel {
                Panel::Chat => {
                    let addr = if self.config.user.preferred_address.is_empty() {
                        self.config.user.name.clone()
                    } else {
                        self.config.user.preferred_address.clone()
                    };
                    let messages = self.short_term.messages();
                    let ai_name = self.config.ai.ai_name.clone();
                    if let Some(text) = self.chat_panel.show(ui, &messages, &ai_name, &addr) {
                        self.submit_message(text);
                    }
                }
                Panel::Settings => {
                    let label = self.secret_store.backend_label().to_string();
                    let (save_cfg, save_sec) =
                        SettingsPanel::show(ui, &mut self.config, &mut self.settings_fields, &label);

                    if save_cfg {
                        match self.config.save() {
                            Ok(_) => self.chat_panel.status_line = "Settings saved.".into(),
                            Err(e) => self.chat_panel.status_line = format!("Save error: {e}"),
                        }
                    }
                    if save_sec {
                        self.secrets = self.settings_fields.into_secrets();
                        match self.secret_store.save(&self.secrets) {
                            Ok(_) => {
                                self.chat_panel.status_line = "API keys saved securely.".into();
                                let ws = WorkerSecrets::from_secrets(&self.secrets);
                                let _ = self.secrets_tx.send(ws);
                            }
                            Err(e) => self.chat_panel.status_line = format!("Key save error: {e}"),
                        }
                    }
                }
            }
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.settings_fields.clear();
        let _ = self.worker_tx.send(WorkerRequest::Shutdown);
    }
}
