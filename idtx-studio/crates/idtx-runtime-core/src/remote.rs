//! Runtime resolver bootstrap and origin-scoped credential arming.

use std::path::Path;
use crate::RunConfig;

pub(crate) fn is_remote(stage: &Path) -> bool {
    stage.to_str().is_some_and(|s| {
        let s = s.trim_start();
        s.starts_with("http://") || s.starts_with("https://")
    })
}

/// Install the resolver before any USD use. Device callers supply their
/// extracted plugin directory; desktop discovers the bundled/dev layout.
pub(crate) fn prepare(cfg: &RunConfig, resolver_plugin_dir: Option<&Path>) {
    idtx_remote::auth::install_shared_refresh();
    idtx_usd_resolver_http::set_network_policy(cfg.asset_policy());
    let name = cfg.project_name.as_deref().unwrap_or_default();
    let cache_dir = idtx_remote::project_cache_dir(name).ok()
        .filter(|d| std::fs::create_dir_all(d).is_ok())
        .unwrap_or_else(|| idtx_remote::project_cache_dir_in(&std::env::temp_dir(), name));
    let installed = match resolver_plugin_dir {
        Some(dir) if dir.join("plugInfo.json").is_file() =>
            idtx_usd_resolver_http::install_with_plugin_dir(&cache_dir, dir),
        _ => idtx_usd_resolver_http::install(&cache_dir),
    };
    if let Err(error) = installed { tracing::warn!(%error, "HTTP USD resolver install failed"); }

    if let Err(error) = arm_credentials(cfg) {
        tracing::warn!(%error, "runtime credentials not armed");
    }
}

/// Explicit token injection for a remote CLI entry authorizes that entry's
/// origin. Embedded tokens authorize only the manifest's `auth_origins`.
/// Network allowlists never enlarge either credential-sharing group.
fn credential_scopes(cfg: &RunConfig, injected: bool) -> Result<idtx_auth::AuthScopes, idtx_auth::AuthError> {
    let mut origins = cfg.auth_origins.clone();
    if injected && is_remote(&cfg.stage) {
        origins.push(idtx_auth::Origin::from_url(&cfg.stage.to_string_lossy())?.to_string());
    }
    idtx_auth::AuthScopes::parse(&origins)
}

fn arm_credentials(cfg: &RunConfig) -> Result<(), String> {
    let explicit = cfg.resolved_token()?;
    let injected = explicit.is_some();
    let token = explicit.or_else(|| cfg.dev_bearer_token.clone());
    if let Some(token) = token {
        return credential_scopes(cfg, injected).map_err(|e| e.to_string())?
            .set_bearer_token(token).map_err(|e| e.to_string());
    }
    let origin = if is_remote(&cfg.stage) {
        idtx_auth::Origin::from_url(&cfg.stage.to_string_lossy()).map_err(|e| e.to_string())?.to_string()
    } else {
        // A local preview can reference the configured server. Its explicit
        // auth group permits selecting that server, but the config credential
        // is still granted only to the config provider's own exact origin.
        let scopes = credential_scopes(cfg, false).map_err(|e| e.to_string())?;
        if scopes.is_empty() { return Ok(()); }
        let Some(server) = idtx_remote::load_config().map_err(|e| e.to_string())? else { return Ok(()); };
        let origin = idtx_auth::Origin::from_url(&server.url).map_err(|e| e.to_string())?.to_string();
        if !scopes.permits(&idtx_auth::AuthScopes::parse(std::slice::from_ref(&origin)).map_err(|e| e.to_string())?) {
            return Ok(());
        }
        origin
    };
    let registry = idtx_remote::AuthRegistry::with_studio_config();
    if let Some(token) = registry.token_for(&origin).map_err(|e| e.to_string())? {
        idtx_auth::set_bearer_token_for_origins(&[origin], token).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(value: &str) -> idtx_auth::AuthScopes {
        idtx_auth::AuthScopes::parse(&[value.into()]).unwrap()
    }

    #[test]
    fn explicit_and_embedded_tokens_have_defined_origin_groups() {
        let mut cfg = RunConfig::new("http://localhost:8080/stage.usda".into(), crate::RunMode::Desktop);
        cfg.auth_origins = vec!["https://*.assets.example:8443".into()];
        cfg.exec_allowed_hosts = vec!["unrelated.example".into()];
        let embedded = credential_scopes(&cfg, false).unwrap();
        assert!(embedded.permits(&scope("https://cdn.assets.example:8443")));
        assert!(!embedded.permits(&scope("https://cdn.assets.example")));
        assert!(!embedded.permits(&scope("https://unrelated.example")));
        assert!(!embedded.permits(&scope("http://localhost:8080")));
        let injected = credential_scopes(&cfg, true).unwrap();
        assert!(injected.permits(&scope("http://localhost:8080")));
        assert!(!injected.permits(&scope("http://localhost:8081")));
        cfg.auth_origins.clear();
        assert!(credential_scopes(&cfg, false).unwrap().is_empty());
        cfg.auth_origins.push("https://bad.example/path".into());
        assert!(credential_scopes(&cfg, false).is_err());
    }

    #[test]
    fn remote_entry_does_not_bypass_network_policy() {
        assert!(is_remote(Path::new("https://assets.example/stage.usda")));
        assert!(is_remote(Path::new("http://assets.example/stage.usda")));
        assert!(!is_remote(Path::new("/local/http://not-a-url.usda")));
        assert!(!is_remote(Path::new("C:\\scene.usda")));
        let mut cfg = RunConfig::new("https://assets.example/stage.usda".into(), crate::RunMode::Desktop);
        assert!(cfg.asset_policy().is_denied());
        cfg.exec_allowed_hosts.push("assets.example".into());
        assert!(cfg.asset_policy().is_denied());
        cfg.exec_allow_network = true;
        assert!(!cfg.asset_policy().is_denied());
    }
}
