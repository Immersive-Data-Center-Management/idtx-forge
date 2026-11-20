//! Rust-side default **auth** environment provider for the `OpenExec`
//! `Compute_Environment` node.
//!
//! This registers a provider under the `"auth"` prefix in the exec plugin's
//! `idtx::EnvironmentProviderRegistry`, so a stage authoring
//! `inputs:key = "auth:TOKEN"` (or `"auth:USERCRED"`) resolves to a ready-to-use
//! non-secret credential reference. The REST request boundary resolves that
//! reference against its actual URL through [`idtx_auth`], on every poll.
//!
//! The registration mechanics (resolving the plugin's exported C-ABI symbol via
//! `libloading`, the resolve trampoline, idempotency, leaking for the process
//! lifetime) live in [`crate::env`]; this module only supplies the `auth:`
//! [`EnvProvider`] and its default registration entry points.

use crate::env::{self, EnvProvider};
use std::path::Path;

/// The routing prefix this provider is registered under; an authored
/// `inputs:key = "auth:TOKEN"` is routed here with key `TOKEN`.
pub const AUTH_PREFIX: &str = "auth";

/// Graph values carry references, never raw credentials. This also makes graph
/// fan-out safe: each REST destination independently resolves its own origin.
struct AuthProvider;

impl EnvProvider for AuthProvider {
    fn resolve(&self, key: &str) -> Option<String> {
        idtx_auth::credential_reference(key)
    }
}

/// Register the default auth provider into the resident plugin at `dir`.
///
/// Idempotent and best-effort — see [`env::register_provider_at`]. Every host
/// that registers the plugin (desktop/XR runtime, Android, visionOS, the
/// editor's Compute Graph preview) gets this for free via [`crate::install`].
pub fn register_default_auth_provider_at(dir: &Path) {
    #[cfg(feature = "metrics")]
    let start = std::time::Instant::now();

    if env::register_provider_at(dir, AUTH_PREFIX, AuthProvider) {
        #[cfg(feature = "metrics")]
        tracing::info!(
            ms = start.elapsed().as_secs_f64() * 1e3,
            "registered default OpenExec auth provider"
        );
    }
}

/// Register the default auth provider, resolving the plugin from the directory
/// [`crate::install`] uses ([`crate::plugin_dir`]). For callers/tests that did
/// not go through a staged [`crate::install_at`].
pub fn register_default_auth_provider() {
    register_default_auth_provider_at(&crate::plugin_dir());
}
