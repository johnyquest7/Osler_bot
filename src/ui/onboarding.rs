/// First-run onboarding wizard.
///
/// Shown when `AppConfig::first_run == true`.  Collects:
///   - User's real name
///   - Preferred form of address
///   - A name for the AI
///   - (optional) free-form user context for the system prompt

use egui::{Align, Button, Color32, Label, Layout, RichText, TextEdit, Ui, Window};

use crate::config::AppConfig;

pub struct Onboarding {
    pub completed: bool,
    name: String,
    address: String,
    ai_name: String,
    about: String,
    step: Step,
}

#[derive(PartialEq)]
enum Step {
    UserName,
    AiName,
    About,
}

impl Onboarding {
    pub fn new() -> Self {
        Self {
            completed: false,
            name: String::new(),
            address: String::new(),
            ai_name: "Osler".into(),
            about: String::new(),
            step: Step::UserName,
        }
    }

    /// Draw the wizard window.  Returns `true` once the user finishes.
    pub fn show(&mut self, ctx: &egui::Context, config: &mut AppConfig) -> bool {
        let mut finished = false;

        Window::new("Welcome – First-Time Setup")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.add_space(8.0);

                match self.step {
                    Step::UserName => self.show_user_name(ui),
                    Step::AiName => self.show_ai_name(ui),
                    Step::About => {
                        finished = self.show_about(ui, config);
                    }
                }
            });

        finished
    }

    fn show_user_name(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("What's your name?").size(16.0).strong());
        ui.add_space(4.0);
        ui.label("This helps your assistant address you personally.");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Your name:");
            ui.text_edit_singleline(&mut self.name);
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Address me as:");
            ui.text_edit_singleline(&mut self.address);
        });
        ui.label(
            RichText::new("e.g. your first name, a nickname, or just leave blank")
                .small()
                .color(Color32::GRAY),
        );
        ui.add_space(12.0);

        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
            if ui
                .add_enabled(!self.name.is_empty(), Button::new("Next →"))
                .clicked()
            {
                self.step = Step::AiName;
            }
        });
    }

    fn show_ai_name(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Name your assistant").size(16.0).strong());
        ui.add_space(4.0);
        ui.label("What would you like to call your AI assistant?");
        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label("Assistant name:");
            ui.text_edit_singleline(&mut self.ai_name);
        });
        ui.add_space(12.0);

        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
            if ui
                .add_enabled(!self.ai_name.is_empty(), Button::new("Next →"))
                .clicked()
            {
                self.step = Step::About;
            }
            if ui.button("← Back").clicked() {
                self.step = Step::UserName;
            }
        });
    }

    fn show_about(&mut self, ui: &mut Ui, config: &mut AppConfig) -> bool {
        let mut finished = false;

        ui.label(
            RichText::new("Anything else the assistant should know?")
                .size(16.0)
                .strong(),
        );
        ui.add_space(4.0);
        ui.label(
            "Optionally provide background context – your profession, preferences, \
             ongoing projects, timezone, etc.  This is injected into every system prompt.",
        );
        ui.add_space(8.0);

        ui.add(
            TextEdit::multiline(&mut self.about)
                .hint_text("e.g. I'm a software engineer based in Helsinki, working on a Rust project...")
                .desired_rows(5)
                .desired_width(f32::INFINITY),
        );
        ui.add_space(12.0);

        ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
            if ui.button("Finish ✓").clicked() {
                config.user.name = self.name.clone();
                config.user.preferred_address = if self.address.is_empty() {
                    self.name.clone()
                } else {
                    self.address.clone()
                };
                config.user.about = self.about.clone();
                config.ai.ai_name = self.ai_name.clone();
                config.first_run = false;
                finished = true;
            }
            if ui.button("← Back").clicked() {
                self.step = Step::AiName;
            }
        });

        finished
    }
}
