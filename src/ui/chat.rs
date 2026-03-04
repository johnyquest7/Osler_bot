/// Chat panel – the main conversation UI.

use egui::{
    Align, Button, Color32, FontId, Frame, Key, Label, Layout, Margin, RichText, ScrollArea,
    Stroke, TextEdit, Ui,
};

use crate::memory::short_term::{ChatMessage, Role};

/// State owned by the App that the chat panel reads and mutates.
pub struct ChatPanel {
    pub input: String,
    pub waiting: bool,
    pub status_line: String,
}

impl ChatPanel {
    pub fn new() -> Self {
        Self {
            input: String::new(),
            waiting: false,
            status_line: String::new(),
        }
    }

    /// Render the chat panel. Returns `Some(text)` when the user submits a
    /// message, `None` otherwise.
    pub fn show(
        &mut self,
        ui: &mut Ui,
        messages: &[ChatMessage],
        ai_name: &str,
        user_name: &str,
    ) -> Option<String> {
        let mut submitted: Option<String> = None;

        // ── Message history ────────────────────────────────────────────────
        let available_height = ui.available_height() - 90.0;

        ScrollArea::vertical()
            .id_salt("chat_scroll")
            .max_height(available_height)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                ui.add_space(4.0);
                for msg in messages {
                    match msg.role {
                        Role::System => {}
                        Role::User => draw_bubble(ui, &msg.content, true, user_name),
                        Role::Assistant => draw_bubble(ui, &msg.content, false, ai_name),
                        Role::Tool => {
                            let label = msg
                                .tool_name
                                .as_deref()
                                .unwrap_or("tool");
                            draw_tool_result(ui, label, &msg.content);
                        }
                    }
                    ui.add_space(6.0);
                }

                if self.waiting {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(format!("{ai_name} is thinking…"))
                                .italics()
                                .color(Color32::GRAY),
                        );
                    });
                }
            });

        ui.separator();

        // ── Status line ────────────────────────────────────────────────────
        if !self.status_line.is_empty() {
            ui.label(RichText::new(&self.status_line).small().color(Color32::GRAY));
        }

        // ── Input row ──────────────────────────────────────────────────────
        ui.horizontal(|ui| {
            let input_id = ui.make_persistent_id("chat_input");
            let resp = ui.add(
                TextEdit::multiline(&mut self.input)
                    .id(input_id)
                    .hint_text("Type a message… (Shift+Enter for newline, Enter to send)")
                    .desired_rows(3)
                    .desired_width(ui.available_width() - 70.0),
            );

            // Enter submits; Shift+Enter inserts newline
            let send_key = resp.has_focus()
                && ui.input(|i| i.key_pressed(Key::Enter) && !i.modifiers.shift);

            let send_btn = ui
                .add_enabled(
                    !self.waiting && !self.input.trim().is_empty(),
                    Button::new("Send").min_size([60.0, 60.0].into()),
                )
                .clicked();

            if (send_key || send_btn) && !self.waiting && !self.input.trim().is_empty() {
                let text = self.input.trim().to_string();
                self.input.clear();
                submitted = Some(text);
            }
        });

        submitted
    }
}

// ─── Bubble rendering helpers ─────────────────────────────────────────────────

fn draw_bubble(ui: &mut Ui, text: &str, is_user: bool, name: &str) {
    let (bg, fg, align) = if is_user {
        (
            Color32::from_rgb(0, 92, 175),
            Color32::WHITE,
            Align::RIGHT,
        )
    } else {
        (
            Color32::from_rgb(45, 45, 48),
            Color32::from_rgb(220, 220, 220),
            Align::LEFT,
        )
    };

    ui.with_layout(Layout::top_down(align), |ui| {
        ui.label(
            RichText::new(name)
                .small()
                .color(Color32::GRAY),
        );

        Frame::none()
            .fill(bg)
            .inner_margin(Margin::symmetric(10.0, 6.0))
            .rounding(8.0)
            .show(ui, |ui| {
                // Wrap long text
                ui.set_max_width(ui.available_width().min(520.0));
                ui.add(Label::new(RichText::new(text).color(fg)));
            });
    });
}

fn draw_tool_result(ui: &mut Ui, tool_name: &str, content: &str) {
    Frame::none()
        .fill(Color32::from_rgb(30, 30, 30))
        .stroke(Stroke::new(1.0, Color32::from_rgb(80, 80, 80)))
        .inner_margin(Margin::symmetric(8.0, 4.0))
        .rounding(4.0)
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!("⚙ tool: {tool_name}"))
                    .small()
                    .color(Color32::from_rgb(180, 140, 50)),
            );
            ui.add(
                Label::new(
                    RichText::new(content)
                        .font(FontId::monospace(11.0))
                        .color(Color32::from_rgb(160, 200, 160)),
                ),
            );
        });
}
