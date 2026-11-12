use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::ConfigError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub url: String,
    pub username: String,
    /// Lookup key for the password. Format: `keychain:<service>/<account>`.
    /// Never stores the raw password.
    pub password_hint: String,
}

// ── internal TOML file shape ──────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct ConfigFile {
    server: Option<ServerSection>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ServerSection {
    url: String,
    username: String,
    password_hint: String,
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn config_path() -> Result<PathBuf, ConfigError> {
    let home = dirs::home_dir().ok_or(ConfigError::NoHomeDir)?;
    Ok(home.join(".idtx-studio").join("config.toml"))
}

/// Directory used to cache assets fetched by the HTTP USD resolver:
/// `~/.idtx-studio/usd-cache`.
///
/// Centralised here so the shell's resolver-install wiring and any future
/// cache-management UI agree on the location. The directory is not created by
/// this call; the resolver creates it on first fetch.
pub fn cache_dir() -> Result<PathBuf, ConfigError> {
    let home = dirs::home_dir().ok_or(ConfigError::NoHomeDir)?;
    Ok(home.join(".idtx-studio").join("usd-cache"))
}

// ── public API ────────────────────────────────────────────────────────────────

/// Reads `~/.idtx-studio/config.toml`. Returns `None` if the file does not
/// exist or has no `[server]` section.
pub fn load_config() -> Result<Option<ServerConfig>, ConfigError> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let text = std::fs::read_to_string(&path)?;
    let file: ConfigFile = toml::from_str(&text)?;
    Ok(file.server.map(|s| ServerConfig {
        url: s.url,
        username: s.username,
        password_hint: s.password_hint,
    }))
}

/// Persists `cfg` to `~/.idtx-studio/config.toml` and stores `password` in
/// the system keychain. The config file never contains the raw password.
pub fn save_config(cfg: &ServerConfig, password: &str) -> Result<(), ConfigError> {
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    // Derive keychain service/account from username and canonical hint format.
    let service = "idtx-studio";
    let account = &cfg.username;

    keyring::Entry::new(service, account)?.set_password(password)?;

    let hint = format!("keychain:{service}/{account}");
    let file = ConfigFile {
        server: Some(ServerSection {
            url: cfg.url.clone(),
            username: cfg.username.clone(),
            password_hint: hint,
        }),
    };

    let text = toml::to_string_pretty(&file)?;
    std::fs::write(&path, text)?;
    debug!(path = %path.display(), "server config saved");
    Ok(())
}

/// Resolves a `password_hint` to the actual password.
///
/// Supported formats:
/// - `keychain:<service>/<account>` — read from the system keychain
/// - anything else — returned as-is (for testing only)
pub fn resolve_password(hint: &str) -> Result<String, ConfigError> {
    if let Some(rest) = hint.strip_prefix("keychain:") {
        let (service, account) = rest
            .split_once('/')
            .ok_or_else(|| ConfigError::BadHint(hint.to_owned()))?;
        let pw = keyring::Entry::new(service, account)?.get_password()?;
        return Ok(pw);
    }
    // Plaintext fallback — only for development/testing.
    Ok(hint.to_owned())
}
