use keyring;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("could not locate home directory")]
    NoHomeDir,
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("TOML parse error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("TOML serialize error: {0}")]
    TomlSer(#[from] toml::ser::Error),
    #[error("keychain error: {0}")]
    Keychain(String),
    #[error("malformed password hint: {0}")]
    BadHint(String),
}

impl From<keyring::Error> for ConfigError {
    fn from(e: keyring::Error) -> Self {
        Self::Keychain(e.to_string())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    #[error("HTTP client error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("server returned {status}: {body}")]
    Server { status: u16, body: String },
    #[error("unexpected response shape: {0}")]
    Parse(String),
    #[error("authentication failed")]
    Unauthorized,
}
