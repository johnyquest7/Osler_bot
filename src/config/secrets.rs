/// Secure storage for all sensitive credentials.
///
/// Strategy:
///   1. Try the OS keyring (libsecret on Linux, Keychain on macOS, Credential
///      Manager on Windows) – most secure.
///   2. If the keyring is unavailable (headless server, minimal install, …),
///      fall back to an AES-256-GCM encrypted JSON file at
///      ~/.config/osler/secrets.enc, with the encryption key stored in a
///      0600-permission keyfile at ~/.config/osler/.key.
///
/// API keys are wrapped with `secrecy::SecretString` so they are:
///   - Never printed in Debug output
///   - Zeroed in memory on drop
///   - Never accidentally passed into AI chat context

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use zeroize::Zeroize;

use crate::config::config_dir;

const APP_NAME: &str = "osler";
const KEYRING_SERVICE: &str = "osler-secrets";

/// Plain-string view of secrets for the async AI worker.
///
/// `SecretString` is intentionally `!Clone` to prevent accidental copying.
/// When we need to pass secrets to an async task (which requires `Send + Clone`),
/// we extract them to this struct on the main thread and move it into the task.
/// The struct is kept in memory only for the duration of the HTTP request.
#[derive(Clone, Default)]
pub struct WorkerSecrets {
    pub openai_api_key: Option<String>,
    pub anthropic_api_key: Option<String>,
    pub gemini_api_key: Option<String>,
    pub telegram_bot_token: Option<String>,
    pub search_api_key: Option<String>,
}

impl WorkerSecrets {
    pub fn from_secrets(s: &Secrets) -> Self {
        let take = |opt: &Option<SecretString>| -> Option<String> {
            opt.as_ref().map(|k| k.expose_secret().to_string())
        };
        Self {
            openai_api_key: take(&s.openai_api_key),
            anthropic_api_key: take(&s.anthropic_api_key),
            gemini_api_key: take(&s.gemini_api_key),
            telegram_bot_token: take(&s.telegram_bot_token),
            search_api_key: take(&s.search_api_key),
        }
    }
}

impl Drop for WorkerSecrets {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        if let Some(k) = &mut self.openai_api_key { k.zeroize(); }
        if let Some(k) = &mut self.anthropic_api_key { k.zeroize(); }
        if let Some(k) = &mut self.gemini_api_key { k.zeroize(); }
        if let Some(k) = &mut self.telegram_bot_token { k.zeroize(); }
        if let Some(k) = &mut self.search_api_key { k.zeroize(); }
    }
}

/// All application secrets in one place.
#[derive(Default)]
pub struct Secrets {
    pub openai_api_key: Option<SecretString>,
    pub anthropic_api_key: Option<SecretString>,
    pub gemini_api_key: Option<SecretString>,
    pub telegram_bot_token: Option<SecretString>,
    /// Optional search API key (Brave Search or SerpAPI)
    pub search_api_key: Option<SecretString>,
}

/// Serialisable version used only for encrypted-file storage.
/// Never written to disk unencrypted.
#[derive(Serialize, Deserialize, Default, Zeroize)]
struct SecretsPlain {
    openai_api_key: String,
    anthropic_api_key: String,
    gemini_api_key: String,
    telegram_bot_token: String,
    search_api_key: String,
}

impl From<&Secrets> for SecretsPlain {
    fn from(s: &Secrets) -> Self {
        SecretsPlain {
            openai_api_key: s
                .openai_api_key
                .as_ref()
                .map(|k| k.expose_secret().to_string())
                .unwrap_or_default(),
            anthropic_api_key: s
                .anthropic_api_key
                .as_ref()
                .map(|k| k.expose_secret().to_string())
                .unwrap_or_default(),
            gemini_api_key: s
                .gemini_api_key
                .as_ref()
                .map(|k| k.expose_secret().to_string())
                .unwrap_or_default(),
            telegram_bot_token: s
                .telegram_bot_token
                .as_ref()
                .map(|k| k.expose_secret().to_string())
                .unwrap_or_default(),
            search_api_key: s
                .search_api_key
                .as_ref()
                .map(|k| k.expose_secret().to_string())
                .unwrap_or_default(),
        }
    }
}

impl From<SecretsPlain> for Secrets {
    fn from(mut p: SecretsPlain) -> Self {
        let make = |s: &str| -> Option<SecretString> {
            if s.is_empty() {
                None
            } else {
                Some(SecretString::new(s.into()))
            }
        };
        let result = Secrets {
            openai_api_key: make(&p.openai_api_key),
            anthropic_api_key: make(&p.anthropic_api_key),
            gemini_api_key: make(&p.gemini_api_key),
            telegram_bot_token: make(&p.telegram_bot_token),
            search_api_key: make(&p.search_api_key),
        };
        p.zeroize();
        result
    }
}

// ─── SecretStore ─────────────────────────────────────────────────────────────

/// Handles loading and saving `Secrets` using the best available backend.
pub struct SecretStore {
    backend: Backend,
}

enum Backend {
    Keyring,
    EncryptedFile { key_path: PathBuf, enc_path: PathBuf },
}

impl SecretStore {
    /// Initialise the store, choosing keyring or encrypted-file.
    pub fn new() -> Self {
        if keyring_available() {
            tracing::info!("Using OS keyring for secret storage");
            SecretStore { backend: Backend::Keyring }
        } else {
            tracing::warn!(
                "OS keyring unavailable – falling back to encrypted-file storage"
            );
            let dir = config_dir();
            SecretStore {
                backend: Backend::EncryptedFile {
                    key_path: dir.join(".key"),
                    enc_path: dir.join("secrets.enc"),
                },
            }
        }
    }

    pub fn load(&self) -> Result<Secrets> {
        match &self.backend {
            Backend::Keyring => load_from_keyring(),
            Backend::EncryptedFile { key_path, enc_path } => {
                load_from_file(key_path, enc_path)
            }
        }
    }

    pub fn save(&self, secrets: &Secrets) -> Result<()> {
        match &self.backend {
            Backend::Keyring => save_to_keyring(secrets),
            Backend::EncryptedFile { key_path, enc_path } => {
                save_to_file(key_path, enc_path, secrets)
            }
        }
    }

    /// Returns a human-readable label for the UI status bar.
    pub fn backend_label(&self) -> &str {
        match &self.backend {
            Backend::Keyring => "OS Keyring",
            Backend::EncryptedFile { .. } => "Encrypted File (fallback)",
        }
    }
}

// ─── Keyring backend ─────────────────────────────────────────────────────────

fn keyring_available() -> bool {
    // Try a harmless round-trip to detect if the keyring daemon is running.
    let probe = keyring::Entry::new(KEYRING_SERVICE, "_probe");
    match probe {
        Ok(e) => {
            // set + delete a dummy value
            let _ = e.set_password("probe");
            let _ = e.delete_credential();
            true
        }
        Err(_) => false,
    }
}

fn keyring_entry(field: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(KEYRING_SERVICE, field)
        .with_context(|| format!("Creating keyring entry for '{field}'"))
}

fn kv_get(field: &str) -> Option<SecretString> {
    keyring_entry(field)
        .ok()
        .and_then(|e| e.get_password().ok())
        .filter(|s| !s.is_empty())
        .map(|s| SecretString::new(s.into_boxed_str()))
}

fn kv_set(field: &str, value: &SecretString) -> Result<()> {
    keyring_entry(field)?
        .set_password(value.expose_secret().as_ref())
        .with_context(|| format!("Saving secret '{field}' to keyring"))
}

fn load_from_keyring() -> Result<Secrets> {
    Ok(Secrets {
        openai_api_key: kv_get("openai_api_key"),
        anthropic_api_key: kv_get("anthropic_api_key"),
        gemini_api_key: kv_get("gemini_api_key"),
        telegram_bot_token: kv_get("telegram_bot_token"),
        search_api_key: kv_get("search_api_key"),
    })
}

fn save_to_keyring(s: &Secrets) -> Result<()> {
    // Helper: write if Some, delete if None
    let write_opt = |field: &str, val: &Option<SecretString>| -> Result<()> {
        match val {
            Some(v) => kv_set(field, v),
            None => {
                if let Ok(entry) = keyring_entry(field) {
                    let _ = entry.delete_credential(); // ignore "not found" errors
                }
                Ok(())
            }
        }
    };

    write_opt("openai_api_key", &s.openai_api_key)?;
    write_opt("anthropic_api_key", &s.anthropic_api_key)?;
    write_opt("gemini_api_key", &s.gemini_api_key)?;
    write_opt("telegram_bot_token", &s.telegram_bot_token)?;
    write_opt("search_api_key", &s.search_api_key)?;
    Ok(())
}

// ─── Encrypted-file backend ───────────────────────────────────────────────────

/// On-disk envelope: base64-encoded nonce ++ ciphertext.
#[derive(Serialize, Deserialize)]
struct EncEnvelope {
    nonce_b64: String,
    cipher_b64: String,
}

/// Load or create the 32-byte encryption key stored in `key_path`.
fn ensure_file_key(key_path: &PathBuf) -> Result<[u8; 32]> {
    let dir = key_path.parent().unwrap();
    std::fs::create_dir_all(dir)?;

    if key_path.exists() {
        let raw = std::fs::read(key_path)
            .with_context(|| "Reading secrets key file")?;
        if raw.len() != 32 {
            anyhow::bail!("Key file is corrupt (wrong length)");
        }
        let mut key = [0u8; 32];
        key.copy_from_slice(&raw);
        Ok(key)
    } else {
        // Generate a fresh key
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        std::fs::write(key_path, &key)
            .with_context(|| "Writing secrets key file")?;
        // Restrict permissions to owner-read-write only
        std::fs::set_permissions(key_path, std::fs::Permissions::from_mode(0o600))?;
        tracing::info!("Generated new secrets encryption key at {}", key_path.display());
        Ok(key)
    }
}

fn load_from_file(key_path: &PathBuf, enc_path: &PathBuf) -> Result<Secrets> {
    if !enc_path.exists() {
        return Ok(Secrets::default());
    }

    let raw_key = ensure_file_key(key_path)?;
    let cipher_key = Key::<Aes256Gcm>::from_slice(&raw_key);
    let cipher = Aes256Gcm::new(cipher_key);

    let envelope_text = std::fs::read_to_string(enc_path)
        .with_context(|| "Reading secrets.enc")?;
    let envelope: EncEnvelope =
        serde_json::from_str(&envelope_text).with_context(|| "Parsing secrets.enc")?;

    let nonce_bytes = B64
        .decode(&envelope.nonce_b64)
        .with_context(|| "Decoding nonce")?;
    let cipher_bytes = B64
        .decode(&envelope.cipher_b64)
        .with_context(|| "Decoding ciphertext")?;

    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher
        .decrypt(nonce, cipher_bytes.as_ref())
        .map_err(|_| anyhow::anyhow!("Decryption failed – secrets file may be corrupt"))?;

    let mut plain: SecretsPlain =
        serde_json::from_slice(&plaintext).with_context(|| "Deserialising secrets")?;
    // From<SecretsPlain> calls zeroize() internally on the plain struct
    let secrets = Secrets::from(plain);
    Ok(secrets)
}

fn save_to_file(key_path: &PathBuf, enc_path: &PathBuf, secrets: &Secrets) -> Result<()> {
    let raw_key = ensure_file_key(key_path)?;
    let cipher_key = Key::<Aes256Gcm>::from_slice(&raw_key);
    let cipher = Aes256Gcm::new(cipher_key);

    let mut plain = SecretsPlain::from(secrets);
    let plaintext =
        serde_json::to_vec(&plain).with_context(|| "Serialising secrets")?;
    plain.zeroize();

    let mut nonce_bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_ref())
        .map_err(|_| anyhow::anyhow!("Encryption failed"))?;

    let envelope = EncEnvelope {
        nonce_b64: B64.encode(nonce_bytes),
        cipher_b64: B64.encode(&ciphertext),
    };

    let dir = enc_path.parent().unwrap();
    std::fs::create_dir_all(dir)?;
    let text = serde_json::to_string(&envelope)?;
    std::fs::write(enc_path, text)
        .with_context(|| "Writing secrets.enc")?;
    // Restrict to owner-read-write
    std::fs::set_permissions(enc_path, std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}
