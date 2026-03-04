pub mod secrets;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Returns the app's config directory: ~/.config/osler/
pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("osler")
}

/// Returns the app's data directory: ~/.local/share/osler/
pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("osler")
}

// ─── Top-level config ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    /// True until the onboarding wizard has been completed.
    #[serde(default = "default_true")]
    pub first_run: bool,

    pub user: UserProfile,
    pub ai: AiConfig,
    pub telegram: TelegramConfig,
    pub memory: MemoryConfig,
}

fn default_true() -> bool {
    true
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            first_run: true,
            user: UserProfile::default(),
            ai: AiConfig::default(),
            telegram: TelegramConfig::default(),
            memory: MemoryConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let path = config_dir().join("config.toml");
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("Reading config from {}", path.display()))?;
        let cfg: AppConfig =
            toml::from_str(&text).with_context(|| "Parsing config.toml")?;
        Ok(cfg)
    }

    pub fn save(&self) -> Result<()> {
        let dir = config_dir();
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("config.toml");
        let text = toml::to_string_pretty(self)?;
        std::fs::write(&path, text)?;
        Ok(())
    }
}

// ─── User profile ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UserProfile {
    /// Human's real name (used in greetings)
    pub name: String,
    /// How the AI should address the user ("Alex", "boss", "sir", …)
    pub preferred_address: String,
    /// Free-form context injected into every system prompt
    pub about: String,
}

// ─── AI config ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiConfig {
    pub provider: AiProvider,
    /// Model identifier, e.g. "gpt-4o", "claude-3-5-sonnet-20241022", "gemini-1.5-pro"
    pub model: String,
    /// The name the AI goes by (e.g. "Osler")
    pub ai_name: String,
    /// Short personality description injected into the system prompt
    pub personality: String,
}

impl Default for AiConfig {
    fn default() -> Self {
        Self {
            provider: AiProvider::OpenAI,
            model: "gpt-4o-mini".to_string(),
            ai_name: "Osler".to_string(),
            personality: "You are a helpful, concise, and thoughtful assistant.".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum AiProvider {
    #[default]
    OpenAI,
    Anthropic,
    Gemini,
}

impl AiProvider {
    pub fn label(&self) -> &str {
        match self {
            AiProvider::OpenAI => "OpenAI",
            AiProvider::Anthropic => "Anthropic (Claude)",
            AiProvider::Gemini => "Google Gemini",
        }
    }

    /// Default model string for this provider.
    pub fn default_model(&self) -> &str {
        match self {
            AiProvider::OpenAI => "gpt-4o-mini",
            AiProvider::Anthropic => "claude-3-5-haiku-20241022",
            AiProvider::Gemini => "gemini-1.5-flash",
        }
    }

    /// Available models the user can pick from.
    pub fn available_models(&self) -> &[&str] {
        match self {
            AiProvider::OpenAI => &["gpt-4o", "gpt-4o-mini", "gpt-4-turbo", "gpt-3.5-turbo"],
            AiProvider::Anthropic => &[
                "claude-opus-4-6",
                "claude-sonnet-4-6",
                "claude-3-5-sonnet-20241022",
                "claude-3-5-haiku-20241022",
            ],
            AiProvider::Gemini => &["gemini-1.5-pro", "gemini-1.5-flash", "gemini-2.0-flash"],
        }
    }
}

// ─── Telegram config ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TelegramConfig {
    pub enabled: bool,
    /// The single Telegram user ID that is authorised to use the bot.
    pub allowed_user_id: Option<i64>,
}

// ─── Memory config ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    /// How many recent messages to keep in the active context window.
    pub short_term_window: usize,
    /// Whether to persist summaries to the long-term SQLite store.
    pub enable_long_term: bool,
    /// Maximum number of long-term entries to retrieve per query.
    pub long_term_results: usize,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            short_term_window: 20,
            enable_long_term: true,
            long_term_results: 5,
        }
    }
}
