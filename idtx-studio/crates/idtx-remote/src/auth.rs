//! Pluggable authentication for remote (`http(s)://`) USD stages.
//!
//! Roadmap decision **D4** requires that new authentication methods can be added
//! *without touching the runtime core*: the runtime resolves a bearer token
//! **before** it opens a stage, and re-resolves one after a `401` (the
//! [`idtx_usd_resolver_http`] fetch callback records the unauthorized host).
//! Both steps go through an [`AuthProvider`] resolved from an [`AuthRegistry`],
//! so a downstream product can register its own SSO / device-flow / mTLS
//! provider and the core never changes.
//!
//! Tokens are keyed by **HTTP origin** because one packaged experience can pull
//! assets from more than one server (a stage on one host referencing a texture
//! pack on another). The built-in [`StudioConfigProvider`] wraps the existing
//! `~/.idtx-studio/config.toml` + system-keychain + `POST /auth/login` flow and
//! only answers for the origin of its configured server; another scheme or port falls
//! through to the next registered provider (or to no token).
//!
//! Provider calls receive canonical origins, not bare hostnames. Guest login uses
//! the separately authorized `auth_write`/`net` capabilities in the behavior host.

use std::sync::Mutex;

use tracing::debug;

use crate::config::{load_config, resolve_password};
use crate::error::{ConfigError, RemoteError};

/// Install the same bounded refresh provider for every runtime adapter. The
/// provider still answers only for its configured exact origin; packaged users
/// without a local config return to interactive login after a rejected token.
pub fn install_shared_refresh() {
    let registry = AuthRegistry::with_studio_config();
    idtx_auth::set_refresh_handler(Some(std::sync::Arc::new(move |origin| registry.refresh(origin).ok().flatten())));
}

/// Error resolving a token through a provider.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error(transparent)]
    Origin(#[from] idtx_auth::AuthError),
    /// Reading `~/.idtx-studio/config.toml` or the keychain failed.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The login/refresh HTTP exchange failed.
    #[error(transparent)]
    Remote(#[from] RemoteError),
}

/// Host of `url` (e.g. `assets.example.com`), or `None` for a non-URL / hostless
/// input. Used for network reachability lists; credential keys use `Origin`.
#[must_use]
pub fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
}

/// A source of bearer tokens for explicitly authorized HTTP origins.
///
/// Resolution happens **before** a stage opens ([`token_for`](AuthProvider::token_for))
/// and again to recover from a `401` ([`refresh`](AuthProvider::refresh)). A
/// provider returns `Ok(None)` for a host it does not handle so the registry can
/// try the next one; `Err` is surfaced only if no provider produced a token.
pub trait AuthProvider: Send + Sync {
    /// Stable identifier for logs / diagnostics (e.g. `"studio-config"`).
    fn id(&self) -> &'static str;

    /// Resolve a bearer token for a canonical HTTP origin (including non-default
    /// port), or `Ok(None)` when this provider does not authorize that origin.
    ///
    /// # Errors
    /// Returns [`AuthError`] if this provider *is* responsible for `host` but the
    /// credential lookup or login exchange failed.
    fn token_for(&self, host: &str) -> Result<Option<String>, AuthError>;

    /// Force re-authentication for `host` after a `401`, bypassing any cached
    /// token. Defaults to [`token_for`](AuthProvider::token_for).
    ///
    /// # Errors
    /// As [`token_for`](AuthProvider::token_for).
    fn refresh(&self, host: &str) -> Result<Option<String>, AuthError> {
        self.token_for(host)
    }
}

/// An ordered set of [`AuthProvider`]s. The first provider to return a token for
/// a host wins; a provider that errors does not mask a later provider that
/// succeeds, but a collected error is surfaced if *no* provider produced a token.
#[derive(Default)]
pub struct AuthRegistry {
    providers: Vec<Box<dyn AuthProvider>>,
}

impl std::fmt::Debug for AuthRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthRegistry")
            .field(
                "providers",
                &self.providers.iter().map(|p| p.id()).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl AuthRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry pre-populated with the built-in [`StudioConfigProvider`], which
    /// reuses the editor's `~/.idtx-studio` server config. This is the default
    /// the standalone runtime uses so a preview authenticates as the same user
    /// as the editor without a token on the command line.
    #[must_use]
    pub fn with_studio_config() -> Self {
        let mut r = Self::new();
        r.register(Box::new(StudioConfigProvider::new()));
        r
    }

    /// Append a provider. Later registrations are consulted after earlier ones.
    pub fn register(&mut self, provider: Box<dyn AuthProvider>) {
        debug!(provider = provider.id(), "auth provider registered");
        self.providers.push(provider);
    }

    /// Resolve a token for a request URL's canonical origin.
    ///
    /// # Errors
    /// Returns the first provider error only if no provider produced a token.
    pub fn token_for(&self, host: &str) -> Result<Option<String>, AuthError> {
        self.resolve(host, false)
    }

    /// Force-refresh a token for the exact unauthorized origin (after a `401`).
    ///
    /// # Errors
    /// As [`token_for`](AuthRegistry::token_for).
    pub fn refresh(&self, host: &str) -> Result<Option<String>, AuthError> {
        self.resolve(host, true)
    }

    fn resolve(&self, host: &str, refresh: bool) -> Result<Option<String>, AuthError> {
        let origin = idtx_auth::Origin::from_url(host)?.to_string();
        let host = origin.as_str();
        let mut first_err: Option<AuthError> = None;
        for p in &self.providers {
            let r = if refresh {
                p.refresh(host)
            } else {
                p.token_for(host)
            };
            match r {
                Ok(Some(token)) => return Ok(Some(token)),
                Ok(None) => {}
                Err(e) => {
                    debug!(provider = p.id(), host, error = %e, "auth provider failed; trying next");
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }
        match first_err {
            Some(e) => Err(e),
            None => Ok(None),
        }
    }
}

/// Built-in provider backed by the shared `~/.idtx-studio/config.toml` + system
/// keychain. Answers only for the configured server's origin; caches the token in
/// memory so repeated `token_for` calls do not re-login, while `refresh` clears
/// the cache and re-authenticates.
#[derive(Debug)]
pub struct StudioConfigProvider {
    cached: Mutex<Option<(String, String)>>,
}

impl StudioConfigProvider {
    /// A provider with an empty token cache.
    #[must_use]
    pub fn new() -> Self {
        Self {
            cached: Mutex::new(None),
        }
    }

    /// Read the config, and if its server origin matches `host`, resolve the
    /// password and log in, caching and returning the token. `Ok(None)` when
    /// there is no config or its origin differs.
    fn login_for(&self, host: &str) -> Result<Option<String>, AuthError> {
        let Some(cfg) = load_config()? else {
            return Ok(None);
        };
        if !same_origin(&cfg.url, host)? {
            return Ok(None);
        }
        let password = resolve_password(&cfg.password_hint)?;
        let token = crate::client::login(&cfg.url, &cfg.username, &password)?;
        *self.cached.lock().expect("token cache poisoned") =
            Some((host.to_string(), token.clone()));
        debug!(url = %cfg.url, user = %cfg.username, "studio-config token resolved");
        Ok(Some(token))
    }
}

fn same_origin(configured_url: &str, target: &str) -> Result<bool, AuthError> {
    Ok(idtx_auth::Origin::from_url(configured_url)? == idtx_auth::Origin::from_url(target)?)
}

impl Default for StudioConfigProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl AuthProvider for StudioConfigProvider {
    fn id(&self) -> &'static str {
        "studio-config"
    }

    fn token_for(&self, host: &str) -> Result<Option<String>, AuthError> {
        let origin = idtx_auth::Origin::from_url(host)?.to_string();
        let host = origin.as_str();
        if let Some((h, t)) = self.cached.lock().expect("token cache poisoned").as_ref()
            && h == host {
                return Ok(Some(t.clone()));
            }
        self.login_for(host)
    }

    fn refresh(&self, host: &str) -> Result<Option<String>, AuthError> {
        let origin = idtx_auth::Origin::from_url(host)?.to_string();
        self.cached.lock().expect("token cache poisoned").take();
        self.login_for(&origin)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_of_extracts_and_tolerates_non_urls() {
        assert_eq!(
            host_of("https://assets.example.com/a.usdz").as_deref(),
            Some("assets.example.com")
        );
        assert_eq!(host_of("/local/scene.usda"), None);
        assert_eq!(host_of("garbage"), None);
    }

    /// A provider that returns a fixed answer per host, recording refresh calls.
    struct FakeProvider {
        id: &'static str,
        answers: std::collections::HashMap<String, Result<Option<String>, ()>>,
        refreshed: Mutex<Vec<String>>,
    }

    impl FakeProvider {
        fn new(id: &'static str) -> Self {
            Self {
                id,
                answers: std::collections::HashMap::new(),
                refreshed: Mutex::new(Vec::new()),
            }
        }
        fn answer(mut self, host: &str, token: Option<&str>) -> Self {
            self.answers
                .insert(host.to_string(), Ok(token.map(str::to_string)));
            self
        }
        fn fail(mut self, host: &str) -> Self {
            self.answers.insert(host.to_string(), Err(()));
            self
        }
    }

    impl AuthProvider for FakeProvider {
        fn id(&self) -> &'static str {
            self.id
        }
        fn token_for(&self, host: &str) -> Result<Option<String>, AuthError> {
            match self.answers.get(host) {
                Some(Ok(t)) => Ok(t.clone()),
                // Reuse a real error variant to stand in for a provider failure.
                Some(Err(())) => Err(AuthError::Remote(RemoteError::Unauthorized)),
                None => Ok(None),
            }
        }
        fn refresh(&self, host: &str) -> Result<Option<String>, AuthError> {
            self.refreshed.lock().unwrap().push(host.to_string());
            self.token_for(host)
        }
    }

    #[test]
    fn first_provider_with_a_token_wins() {
        let mut reg = AuthRegistry::new();
        reg.register(Box::new(FakeProvider::new("a").answer("https://h.example", None)));
        reg.register(Box::new(
            FakeProvider::new("b").answer("https://h.example", Some("tok-b")),
        ));
        reg.register(Box::new(
            FakeProvider::new("c").answer("https://h.example", Some("tok-c")),
        ));
        assert_eq!(
            reg.token_for("https://h.example/path").unwrap().as_deref(),
            Some("tok-b")
        );
    }

    #[test]
    fn no_provider_handles_host_is_ok_none() {
        let mut reg = AuthRegistry::new();
        reg.register(Box::new(FakeProvider::new("a").answer("https://other", Some("x"))));
        assert_eq!(reg.token_for("https://unhandled").unwrap(), None);
    }

    #[test]
    fn provider_error_does_not_mask_later_success() {
        let mut reg = AuthRegistry::new();
        reg.register(Box::new(FakeProvider::new("a").fail("https://h.example")));
        reg.register(Box::new(
            FakeProvider::new("b").answer("https://h.example", Some("tok-b")),
        ));
        assert_eq!(
            reg.token_for("https://h.example").unwrap().as_deref(),
            Some("tok-b")
        );
    }

    #[test]
    fn error_surfaced_when_nothing_succeeds() {
        let mut reg = AuthRegistry::new();
        reg.register(Box::new(FakeProvider::new("a").fail("https://h.example")));
        assert!(reg.token_for("https://h.example").is_err());
    }

    #[test]
    fn refresh_delegates_to_providers() {
        let mut reg = AuthRegistry::new();
        reg.register(Box::new(
            FakeProvider::new("a").answer("https://h.example", Some("tok-a")),
        ));
        assert_eq!(reg.refresh("https://h.example").unwrap().as_deref(), Some("tok-a"));
    }

    #[test]
    fn configured_server_credentials_cannot_cross_scheme_or_port() {
        assert!(same_origin("https://API.example:443/api", "https://api.example/data").unwrap());
        assert!(!same_origin("https://api.example", "http://api.example").unwrap());
        assert!(!same_origin("https://api.example:8443", "https://api.example").unwrap());
        assert!(same_origin("https://api.example", "api.example").is_err());
    }
}
