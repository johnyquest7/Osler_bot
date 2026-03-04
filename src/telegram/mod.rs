/// Telegram bot integration using teloxide Dispatcher.
///
/// The bot is single-user: only the owner's Telegram user ID (set in settings)
/// is permitted to interact with it.  All other messages receive a polite
/// rejection.
///
/// The bot runs as a long-polling background task on the shared tokio runtime
/// and communicates with the AI engine through tokio mpsc channels.

use teloxide::dispatching::Dispatcher;
use teloxide::prelude::*;
use tokio::sync::mpsc;

/// A request sent from the Telegram handler to the AI engine.
#[derive(Debug)]
pub struct TelegramAiRequest {
    pub chat_id: ChatId,
    pub text: String,
}

/// A response sent from the AI engine back to the Telegram handler.
#[derive(Debug)]
pub struct TelegramAiResponse {
    pub chat_id: ChatId,
    pub text: String,
}

/// Launch the Telegram bot listener.
pub async fn run_bot(
    token: String,
    allowed_id: i64,
    ai_request_tx: mpsc::Sender<TelegramAiRequest>,
    mut ai_response_rx: mpsc::Receiver<TelegramAiResponse>,
) {
    tracing::info!("Starting Telegram bot (allowed user id: {allowed_id})");

    let bot = Bot::new(token);
    let bot_for_replies = bot.clone();

    // Spawn a task that reads AI responses and sends them back to Telegram.
    tokio::spawn(async move {
        while let Some(resp) = ai_response_rx.recv().await {
            if let Err(e) = bot_for_replies
                .send_message(resp.chat_id, &resp.text)
                .await
            {
                tracing::error!("Failed to send Telegram reply: {e}");
            }
        }
    });

    // Build a simple message handler
    let handler = Update::filter_message().endpoint(
        move |bot: Bot, msg: Message| {
            let tx = ai_request_tx.clone();
            async move {
                let from_id = msg.from.as_ref().map(|u| u.id.0 as i64).unwrap_or(-1);

                if from_id != allowed_id {
                    tracing::warn!(
                        "Rejected Telegram message from unauthorized user {from_id}"
                    );
                    bot.send_message(msg.chat.id, "Sorry, I'm a private assistant.")
                        .await?;
                    return respond(());
                }

                let text = msg.text().unwrap_or("").trim().to_string();
                if text.is_empty() {
                    return respond(());
                }

                if let Err(e) = tx
                    .send(TelegramAiRequest {
                        chat_id: msg.chat.id,
                        text,
                    })
                    .await
                {
                    tracing::error!("Failed to forward Telegram message to AI: {e}");
                    bot.send_message(msg.chat.id, "Internal error – try again later.")
                        .await?;
                }

                respond(())
            }
        },
    );

    Dispatcher::builder(bot, handler)
        .build()
        .dispatch()
        .await;
}
