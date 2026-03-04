/// Settings panel – configure AI provider, keys, user profile, Telegram, memory.

use egui::{Color32, ComboBox, Grid, RichText, ScrollArea, TextEdit, Ui};

use crate::config::{AiProvider, AppConfig};
use crate::config::secrets::Secrets;

/// Temporary buffer for secrets so we can edit them as plain strings.
/// Cleared from memory when the settings panel is closed.
#[derive(Default)]
pub struct SecretFields {
    pub openai_key: String,
    pub anthropic_key: String,
    pub gemini_key: String,
    pub telegram_token: String,
    pub search_key: String,
    pub show_keys: bool,
}

impl SecretFields {
    /// Populate from the loaded secrets.
    pub fn load_from(&mut self, secrets: &Secrets) {
        use secrecy::ExposeSecret;
        let take = |opt: &Option<secrecy::SecretString>| -> String {
            opt.as_ref()
                .map(|k| k.expose_secret().to_string())
                .unwrap_or_default()
        };
        self.openai_key = take(&secrets.openai_api_key);
        self.anthropic_key = take(&secrets.anthropic_api_key);
        self.gemini_key = take(&secrets.gemini_api_key);
        self.telegram_token = take(&secrets.telegram_bot_token);
        self.search_key = take(&secrets.search_api_key);
    }

    /// Write back into a `Secrets` struct.
    pub fn into_secrets(&self) -> Secrets {
        use secrecy::SecretString;
        let make = |s: &str| -> Option<SecretString> {
            if s.is_empty() { None } else { Some(SecretString::new(s.into())) }
        };
        Secrets {
            openai_api_key: make(&self.openai_key),
            anthropic_api_key: make(&self.anthropic_key),
            gemini_api_key: make(&self.gemini_key),
            telegram_bot_token: make(&self.telegram_token),
            search_api_key: make(&self.search_key),
        }
    }

    /// Zeroize all buffers (call when panel is hidden).
    pub fn clear(&mut self) {
        use zeroize::Zeroize;
        self.openai_key.zeroize();
        self.anthropic_key.zeroize();
        self.gemini_key.zeroize();
        self.telegram_token.zeroize();
        self.search_key.zeroize();
    }
}

pub struct SettingsPanel;

impl SettingsPanel {
    /// Draw the settings panel.
    ///
    /// Returns `(save_config, save_secrets)` flags.
    pub fn show(
        ui: &mut Ui,
        config: &mut AppConfig,
        fields: &mut SecretFields,
        storage_label: &str,
    ) -> (bool, bool) {
        let mut save_config = false;
        let mut save_secrets = false;

        ScrollArea::vertical().show(ui, |ui| {
            // ── AI Model ────────────────────────────────────────────────────
            ui.heading("AI Model");
            ui.add_space(4.0);
            Grid::new("ai_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Provider:");
                ComboBox::from_id_salt("provider_combo")
                    .selected_text(config.ai.provider.label())
                    .show_ui(ui, |ui| {
                        let providers = [AiProvider::OpenAI, AiProvider::Anthropic, AiProvider::Gemini];
                        for p in providers {
                            let label = p.label().to_string();
                            if ui
                                .selectable_value(&mut config.ai.provider, p.clone(), label)
                                .clicked()
                            {
                                config.ai.model = p.default_model().to_string();
                            }
                        }
                    });
                ui.end_row();

                ui.label("Model:");
                ComboBox::from_id_salt("model_combo")
                    .selected_text(&config.ai.model)
                    .show_ui(ui, |ui| {
                        for m in config.ai.provider.available_models() {
                            ui.selectable_value(&mut config.ai.model, m.to_string(), *m);
                        }
                    });
                ui.end_row();

                ui.label("AI name:");
                ui.text_edit_singleline(&mut config.ai.ai_name);
                ui.end_row();

                ui.label("Personality:");
                ui.add(
                    TextEdit::multiline(&mut config.ai.personality)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            });

            ui.add_space(12.0);
            ui.separator();

            // ── API Keys ────────────────────────────────────────────────────
            ui.heading("API Keys");
            ui.label(
                RichText::new(format!("Storage backend: {storage_label}"))
                    .small()
                    .color(Color32::GRAY),
            );
            ui.add_space(4.0);

            ui.checkbox(&mut fields.show_keys, "Show keys");
            ui.add_space(4.0);

            let password_mask = !fields.show_keys;

            Grid::new("keys_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                key_row(ui, "OpenAI key:", &mut fields.openai_key, password_mask);
                key_row(ui, "Anthropic key:", &mut fields.anthropic_key, password_mask);
                key_row(ui, "Gemini key:", &mut fields.gemini_key, password_mask);
                key_row(ui, "Telegram token:", &mut fields.telegram_token, password_mask);
                key_row(ui, "Search API key:", &mut fields.search_key, password_mask);
            });

            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "⚠ API keys are never shared with the AI model or included in chat logs.",
                )
                .small()
                .color(Color32::from_rgb(180, 140, 0)),
            );

            ui.add_space(12.0);
            ui.separator();

            // ── User Profile ────────────────────────────────────────────────
            ui.heading("User Profile");
            ui.add_space(4.0);
            Grid::new("user_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Your name:");
                ui.text_edit_singleline(&mut config.user.name);
                ui.end_row();

                ui.label("Address me as:");
                ui.text_edit_singleline(&mut config.user.preferred_address);
                ui.end_row();

                ui.label("About me:");
                ui.add(
                    TextEdit::multiline(&mut config.user.about)
                        .hint_text("Background context the AI should know about you…")
                        .desired_rows(5)
                        .desired_width(f32::INFINITY),
                );
                ui.end_row();
            });

            ui.add_space(12.0);
            ui.separator();

            // ── Telegram ────────────────────────────────────────────────────
            ui.heading("Telegram");
            ui.add_space(4.0);
            ui.checkbox(&mut config.telegram.enabled, "Enable Telegram bot");

            if config.telegram.enabled {
                ui.add_space(4.0);
                ui.label("The bot token is set in API Keys above.");
                Grid::new("tg_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    ui.label("Allowed Telegram user ID:");
                    let mut id_str = config
                        .telegram
                        .allowed_user_id
                        .map(|id| id.to_string())
                        .unwrap_or_default();
                    ui.text_edit_singleline(&mut id_str);
                    config.telegram.allowed_user_id = id_str.parse::<i64>().ok();
                    ui.end_row();
                });
            }

            ui.add_space(12.0);
            ui.separator();

            // ── Memory ──────────────────────────────────────────────────────
            ui.heading("Memory");
            ui.add_space(4.0);
            Grid::new("mem_grid").num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label("Short-term window:");
                ui.add(egui::Slider::new(&mut config.memory.short_term_window, 5..=100));
                ui.end_row();

                ui.label("Long-term memory:");
                ui.checkbox(&mut config.memory.enable_long_term, "Enabled");
                ui.end_row();

                ui.label("Max results retrieved:");
                ui.add(egui::Slider::new(&mut config.memory.long_term_results, 1..=20));
                ui.end_row();
            });

            ui.add_space(16.0);

            // ── Save buttons ────────────────────────────────────────────────
            ui.horizontal(|ui| {
                if ui.button("Save Settings").clicked() {
                    save_config = true;
                }
                if ui.button("Save API Keys").clicked() {
                    save_secrets = true;
                }
            });
        });

        (save_config, save_secrets)
    }
}

fn key_row(ui: &mut Ui, label: &str, value: &mut String, mask: bool) {
    ui.label(label);
    ui.add(
        TextEdit::singleline(value)
            .password(mask)
            .desired_width(f32::INFINITY),
    );
    ui.end_row();
}
