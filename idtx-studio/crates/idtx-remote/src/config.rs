use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::error::ConfigError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// Human-readable label shown in the configuration table and in the
    /// per-server menu entries (e.g. "Production", "Staging"). Required when
    /// saved through the UI.
    #[serde(default)]
    pub description: String,
    pub url: String,
    pub username: String,
    /// Lookup key for the password. Format: `keychain:<service>/<account>`.
    /// Never stores the raw password.
    pub password_hint: String,
}

// ── internal TOML file shape ──────────────────────────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct ConfigFile {
    /// Legacy single-server section (`[server]`). Read for backward
    /// compatibility and migrated into [`ConfigFile::servers`]; never written
    /// once a save goes through the multi-server path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    server: Option<ServerSection>,
    /// Multi-server list (`[[servers]]`). The canonical shape going forward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    servers: Option<Vec<ServerSection>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct ServerSection {
    /// Absent in legacy single-server files; defaulted from the URL host on
    /// migration so the UI always has a non-empty label.
    #[serde(default)]
    description: String,
    url: String,
    username: String,
    password_hint: String,
}

impl From<ServerSection> for ServerConfig {
    fn from(s: ServerSection) -> Self {
        ServerConfig {
            description: s.description,
            url: s.url,
            username: s.username,
            password_hint: s.password_hint,
        }
    }
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

/// Derive the stable keychain account for a server: `"{username}@{origin}"`.
///
/// Keying by origin (rather than username alone) keeps two servers that share a
/// username from colliding on the same keychain entry.
fn keychain_account(username: &str, url: &str) -> Result<String, ConfigError> {
    let origin =
        idtx_auth::Origin::from_url(url).map_err(|e| ConfigError::BadUrl(e.to_string()))?;
    Ok(format!("{username}@{origin}"))
}

// ── public API ────────────────────────────────────────────────────────────────

/// Reads `~/.idtx-studio/config.toml` and returns every configured server.
///
/// A file written in the new `[[servers]]` form is returned as-is. A legacy
/// file carrying a single `[server]` section is migrated into a one-element
/// list, with `description` defaulted to the URL host so the UI has a label.
/// Returns an empty vec when the file is missing or carries neither section.
pub fn load_configs() -> Result<Vec<ServerConfig>, ConfigError> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let file: ConfigFile = toml::from_str(&text)?;

    if let Some(servers) = file.servers {
        return Ok(servers.into_iter().map(ServerConfig::from).collect());
    }
    // Legacy single `[server]` → migrate. Keep `password_hint` verbatim so the
    // existing keychain entry keeps resolving.
    Ok(file
        .server
        .map(|mut s| {
            if s.description.is_empty() {
                s.description =
                    crate::auth::host_of(&s.url).unwrap_or_else(|| s.url.clone());
            }
            vec![ServerConfig::from(s)]
        })
        .unwrap_or_default())
}

/// Reads `~/.idtx-studio/config.toml`. Returns `None` if no server is
/// configured, else the first configured server.
///
/// Thin wrapper over [`load_configs`] kept for callers that still operate on a
/// single server (the standalone runtime's pre-open auth arming and the
/// keychain-backed [`crate::auth::StudioConfigProvider`]).
pub fn load_config() -> Result<Option<ServerConfig>, ConfigError> {
    Ok(load_configs()?.into_iter().next())
}

/// Persists `servers` to `~/.idtx-studio/config.toml` and stores each password
/// in the system keychain. `passwords[i]` is the raw password for `servers[i]`;
/// the slices must be the same length. The config file never contains a raw
/// password.
///
/// # Panics
/// Panics if `servers.len() != passwords.len()`.
pub fn save_configs(servers: &[ServerConfig], passwords: &[&str]) -> Result<(), ConfigError> {
    assert_eq!(
        servers.len(),
        passwords.len(),
        "save_configs: one password per server"
    );
    let path = config_path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let service = "idtx-studio";
    let mut sections = Vec::with_capacity(servers.len());
    for (cfg, password) in servers.iter().zip(passwords) {
        let account = keychain_account(&cfg.username, &cfg.url)?;
        keyring::Entry::new(service, &account)?.set_password(password)?;
        sections.push(ServerSection {
            description: cfg.description.clone(),
            url: cfg.url.clone(),
            username: cfg.username.clone(),
            password_hint: format!("keychain:{service}/{account}"),
        });
    }

    let file = ConfigFile {
        server: None,
        servers: Some(sections),
    };
    let text = toml::to_string_pretty(&file)?;
    std::fs::write(&path, text)?;
    debug!(path = %path.display(), count = servers.len(), "server config saved");
    Ok(())
}

/// Persists a single `cfg` + `password`. Thin wrapper over [`save_configs`].
pub fn save_config(cfg: &ServerConfig, password: &str) -> Result<(), ConfigError> {
    save_configs(std::slice::from_ref(cfg), &[password])
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multi_server_toml_round_trips() {
        let servers = vec![
            ServerSection {
                description: "Prod".into(),
                url: "https://prod.example.com".into(),
                username: "alice".into(),
                password_hint: "keychain:idtx-studio/alice@https://prod.example.com".into(),
            },
            ServerSection {
                description: "Staging".into(),
                url: "https://staging.example.com".into(),
                username: "bob".into(),
                password_hint: "keychain:idtx-studio/bob@https://staging.example.com".into(),
            },
        ];
        let file = ConfigFile {
            server: None,
            servers: Some(servers),
        };
        let text = toml::to_string_pretty(&file).unwrap();
        // Only the `[[servers]]` array is emitted, never a legacy `[server]`.
        assert!(text.contains("[[servers]]"));
        assert!(!text.contains("\n[server]"));

        let parsed: ConfigFile = toml::from_str(&text).unwrap();
        let list = parsed.servers.unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].description, "Prod");
        assert_eq!(list[1].username, "bob");
    }

    #[test]
    fn legacy_single_server_parses_into_section() {
        // A pre-existing file carries a single `[server]` section with no
        // description field.
        let text = "\
[server]
url = \"https://legacy.example.com\"
username = \"carol\"
password_hint = \"keychain:idtx-studio/carol\"
";
        let parsed: ConfigFile = toml::from_str(text).unwrap();
        assert!(parsed.servers.is_none());
        let legacy = parsed.server.unwrap();
        assert_eq!(legacy.url, "https://legacy.example.com");
        assert_eq!(legacy.description, ""); // defaulted empty, filled on migration
        assert_eq!(legacy.password_hint, "keychain:idtx-studio/carol");
    }

    #[test]
    fn keychain_account_is_user_at_origin() {
        let account = keychain_account("alice", "https://prod.example.com/api").unwrap();
        assert_eq!(account, "alice@https://prod.example.com");
        // The hint survives resolve_password's `split_once('/')` parsing: the
        // service is everything up to the first slash, the account the rest.
        let hint = format!("keychain:idtx-studio/{account}");
        let rest = hint.strip_prefix("keychain:").unwrap();
        let (service, acct) = rest.split_once('/').unwrap();
        assert_eq!(service, "idtx-studio");
        assert_eq!(acct, "alice@https://prod.example.com");
    }
}
