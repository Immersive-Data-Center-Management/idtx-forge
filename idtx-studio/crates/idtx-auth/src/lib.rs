//! Shared origin-scoped credentials for USD assets, behavior HTTP and `OpenExec`.
//!
//! Credentials authorize explicit HTTP(S) origins (scheme, canonical host and
//! effective port), independently of network reachability. There is no global
//! fallback. `https://*.example.com` includes the apex and subdomains on HTTPS
//! port 443 only; exact grants take precedence over wildcard grants. `OpenExec`
//! transports non-secret `auth:*` references and resolves them at request time.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Mutex, OnceLock};

mod refresh;
pub use refresh::{RefreshHandler, set_refresh_handler, refresh_after_unauthorized};
static REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn revision() -> u64 { REVISION.load(std::sync::atomic::Ordering::Acquire) }
fn changed() { REVISION.fetch_add(1, std::sync::atomic::Ordering::Release); }
/// Cancel pending credential publication without revoking current credentials.
pub fn cancel_pending_refresh() { let _guard = lock(); changed(); }

pub const KEY_TOKEN: &str = "TOKEN";
pub const KEY_USERCRED: &str = "USERCRED";
const MAX_SCOPES: usize = 256;
const MAX_SCOPE_BYTES: usize = 512;
const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AuthError {
    #[error("expected an HTTP(S) origin without userinfo, path, query or fragment")]
    InvalidOrigin,
    #[error("no authorized credential origins configured")]
    NoOrigins,
    #[error("credential scope capacity exceeded")]
    Capacity,
    #[error("invalid or oversized credential")]
    InvalidCredential,
}

/// Canonical request origin. Paths/queries are not part of an origin; URL
/// userinfo is rejected so it cannot add a second implicit credential source.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    pub fn from_url(value: &str) -> Result<Self, AuthError> {
        let url = url::Url::parse(value).map_err(|_| AuthError::InvalidOrigin)?;
        Self::from_parsed(&url)
    }

    fn from_parsed(url: &url::Url) -> Result<Self, AuthError> {
        if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() {
            return Err(AuthError::InvalidOrigin);
        }
        let host = url.host_str().ok_or(AuthError::InvalidOrigin)?;
        if host.contains('*') { return Err(AuthError::InvalidOrigin); }
        Ok(Self { scheme: url.scheme().into(), host: host.into(),
            port: url.port_or_known_default().ok_or(AuthError::InvalidOrigin)? })
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme, self.host)?;
        if self.port != if self.scheme == "https" { 443 } else { 80 } {
            write!(f, ":{}", self.port)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct OriginScope {
    origin: Origin,
    wildcard: bool,
}

impl OriginScope {
    fn parse(value: &str) -> Result<Self, AuthError> {
        let value = value.trim();
        if value.len() > MAX_SCOPE_BYTES || value.contains('\\') {
            return Err(AuthError::InvalidOrigin);
        }
        let (scheme, authority) = value.split_once("://").ok_or(AuthError::InvalidOrigin)?;
        let wildcard = authority.starts_with("*.");
        let plain = if wildcard { format!("{scheme}://{}", &authority[2..]) } else { value.to_owned() };
        let url = url::Url::parse(&plain).map_err(|_| AuthError::InvalidOrigin)?;
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err(AuthError::InvalidOrigin);
        }
        let origin = Origin::from_parsed(&url)?;
        if wildcard && !matches!(url.host(), Some(url::Host::Domain(_))) {
            return Err(AuthError::InvalidOrigin);
        }
        Ok(Self { origin, wildcard })
    }

    fn matches(&self, origin: &Origin) -> bool {
        self.origin.scheme == origin.scheme && self.origin.port == origin.port
            && (self.origin.host == origin.host || (self.wildcard
                && origin.host.strip_suffix(&self.origin.host).is_some_and(|p| p.ends_with('.'))))
    }

    fn contains(&self, other: &Self) -> bool {
        (!other.wildcard || self.wildcard) && self.matches(&other.origin)
    }
}

/// Validated authorized sharing group, suitable for a behavior host's writer
/// policy. Empty is deny-all. Network allowlists do not populate this group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuthScopes(Vec<OriginScope>);

impl AuthScopes {
    pub fn parse(values: &[String]) -> Result<Self, AuthError> {
        if values.len() > MAX_SCOPES { return Err(AuthError::Capacity); }
        let mut scopes = values.iter().map(|v| OriginScope::parse(v)).collect::<Result<Vec<_>, _>>()?;
        scopes.sort();
        scopes.dedup();
        Ok(Self(scopes))
    }

    /// Legacy guest `_for_host(s)` selectors: a bare host means HTTPS/default
    /// port. Explicit schemes, ports and wildcard origins use the normal grammar.
    pub fn from_host_selectors(values: &[String]) -> Result<Self, AuthError> {
        Self::parse(&values.iter().map(|v| {
            let v = v.trim();
            if v.contains("://") { v.to_owned() } else { format!("https://{v}") }
        }).collect::<Vec<_>>())
    }

    #[must_use]
    pub fn is_empty(&self) -> bool { self.0.is_empty() }

    #[must_use]
    pub fn permits(&self, requested: &Self) -> bool {
        !requested.is_empty() && requested.0.iter().all(|s| self.0.iter().any(|allowed| allowed.contains(s)))
    }

    pub fn set_bearer_token(&self, token: impl Into<String>) -> Result<(), AuthError> {
        let token = token.into();
        if token.is_empty() || token.len() > MAX_CREDENTIAL_BYTES
            || token.bytes().any(|b| !b.is_ascii() || b <= b' ' || b == 127)
        { return Err(AuthError::InvalidCredential); }
        store(self, &AuthData::Bearer(token))
    }

    pub fn set_user_credentials(&self, user: impl Into<String>, password: impl Into<String>) -> Result<(), AuthError> {
        let (user, password) = (user.into(), password.into());
        if user.contains(':') || user.len().saturating_add(password.len()) > MAX_CREDENTIAL_BYTES {
            return Err(AuthError::InvalidCredential);
        }
        store(self, &AuthData::Basic { user, password })
    }

    /// Revoke this writer's group, including more-specific stored grants.
    pub fn clear(&self) -> Result<(), AuthError> {
        if self.is_empty() { return Err(AuthError::NoOrigins); }
        let mut map = lock();
        let retained = map.keys().filter(|scope| !self.0.iter().any(|allowed| allowed.contains(scope))).count();
        if retained.saturating_add(self.0.len()) > MAX_SCOPES { return Err(AuthError::Capacity); }
        map.retain(|scope, _| !self.0.iter().any(|allowed| allowed.contains(scope)));
        for scope in &self.0 { map.insert(scope.clone(), AuthData::None); }
        changed();
        Ok(())
    }
}

#[derive(Clone)]
enum AuthData { None, Bearer(String), Basic { user: String, password: String } }

fn credentials() -> &'static Mutex<BTreeMap<OriginScope, AuthData>> {
    static STORE: OnceLock<Mutex<BTreeMap<OriginScope, AuthData>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn lock() -> std::sync::MutexGuard<'static, BTreeMap<OriginScope, AuthData>> {
    credentials().lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn store(scopes: &AuthScopes, data: &AuthData) -> Result<(), AuthError> {
    if scopes.is_empty() { return Err(AuthError::NoOrigins); }
    let mut map = lock();
    let additional = scopes.0.iter().filter(|s| !map.contains_key(s)).count();
    if map.len().saturating_add(additional) > MAX_SCOPES { return Err(AuthError::Capacity); }
    for scope in &scopes.0 { map.insert(scope.clone(), data.clone()); }
    changed();
    tracing::debug!(origins = scopes.0.len(), "stored origin-scoped credentials");
    Ok(())
}

pub fn set_bearer_token_for_origins(origins: &[String], token: impl Into<String>) -> Result<(), AuthError> {
    AuthScopes::parse(origins)?.set_bearer_token(token)
}

pub fn set_user_credentials_for_origins(origins: &[String], user: impl Into<String>, password: impl Into<String>) -> Result<(), AuthError> {
    AuthScopes::parse(origins)?.set_user_credentials(user, password)
}

/// Compatibility selector spelling; bare hosts grant only HTTPS/default port.
pub fn set_bearer_token_for_hosts(hosts: &[String], token: impl Into<String>) -> Result<(), AuthError> {
    AuthScopes::from_host_selectors(hosts)?.set_bearer_token(token)
}

pub fn set_user_credentials_for_hosts(hosts: &[String], user: impl Into<String>, password: impl Into<String>) -> Result<(), AuthError> {
    AuthScopes::from_host_selectors(hosts)?.set_user_credentials(user, password)
}

/// Atomically revoke all grants. Every consumer resolves again for each request.
pub fn clear() { let mut map = lock(); map.clear(); changed(); }

fn revoke_rejected(url: &str, rejected: &str, expected_revision: u64) {
    let Ok(origin) = Origin::from_url(url) else { return; };
    let mut map = lock();
    if revision() != expected_revision { return; }
    // Revoke only the single most-specific grant actually resolved for the
    // rejected request origin — the same selection `credential_for` and
    // `publish_refresh` use. A blanket sweep of every grant that happens to
    // hold the same header value would break origin isolation when one token is
    // intentionally shared across independent scopes: a 401 from one origin must
    // not revoke a sibling origin that never returned 401.
    let Some((_, data)) = map.iter_mut().filter(|(scope, _)| scope.matches(&origin))
        .max_by_key(|(scope, _)| (!scope.wildcard, scope.origin.host.len())) else { return; };
    if header_for(data).as_deref() != Some(rejected) { return; }
    *data = AuthData::None;
    changed();
}

fn publish_refresh(url: &str, rejected: &str, expected_revision: u64, token: String) -> bool {
    if token.is_empty() || token.len() > MAX_CREDENTIAL_BYTES || token.bytes().any(|b| !b.is_ascii() || b <= b' ' || b == 127) { return false; }
    let Ok(origin) = Origin::from_url(url) else { return false; };
    let mut map = lock();
    if revision() != expected_revision { return false; }
    let Some((_, data)) = map.iter_mut().filter(|(scope, _)| scope.matches(&origin))
        .max_by_key(|(scope, _)| (!scope.wildcard, scope.origin.host.len())) else { return false; };
    if header_for(data).as_deref() != Some(rejected) { return false; }
    *data = AuthData::Bearer(token);
    changed();
    true
}

/// Revoke one concrete origin, including access through an enclosing wildcard.
pub fn clear_origin(url: &str) -> Result<(), AuthError> {
    let scope = OriginScope { origin: Origin::from_url(url)?, wildcard: false };
    store(&AuthScopes(vec![scope]), &AuthData::None)
}

fn credential_for(url: &str) -> Option<AuthData> {
    let origin = Origin::from_url(url).ok()?;
    lock().iter().filter(|(scope, _)| scope.matches(&origin))
        .max_by_key(|(scope, _)| (!scope.wildcard, scope.origin.host.len()))
        .map(|(_, data)| data.clone())
}

/// Resolve for a complete request URL, never for a bare hostname.
#[must_use]
pub fn authorization_header_for(url: &str) -> Option<String> {
    header_for(&credential_for(url)?)
}

/// Resolve an `OpenExec` credential key with the actual destination URL.
#[must_use]
pub fn resolve_header(name: &str, url: &str) -> Option<String> {
    let data = credential_for(url)?;
    match (name, &data) {
        (KEY_TOKEN, AuthData::Bearer(_)) | (KEY_USERCRED, AuthData::Basic { .. }) => header_for(&data),
        _ => None,
    }
}

/// Non-secret value transported through an `OpenExec` graph. Secrets are resolved
/// only by the REST consumer, after its URL is known, and are never cached in USD.
#[must_use]
pub fn credential_reference(name: &str) -> Option<String> {
    matches!(name, KEY_TOKEN | KEY_USERCRED).then(|| format!("auth:{name}"))
}

/// Interpret a datasource authorization input at send time. Auth references are
/// scoped lookups; an explicitly authored literal header remains a literal.
#[must_use]
pub fn authorization_from_input(value: &str, url: &str) -> Option<String> {
    if let Some(key) = value.strip_prefix("auth:") { resolve_header(key, url) }
    else { (!value.trim().is_empty()).then(|| value.to_owned()) }
}

fn header_for(data: &AuthData) -> Option<String> {
    match data {
        AuthData::None => None,
        AuthData::Bearer(token) => Some(format!("Bearer {token}")),
        AuthData::Basic { user, password } => Some(format!("Basic {}", base64_encode(format!("{user}:{password}").as_bytes()))),
    }
}

fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(chunk.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(chunk.get(2).copied().unwrap_or(0));
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 { ALPHABET[((n >> 6) & 0x3F) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(n & 0x3F) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_parsing_and_delegation_are_origin_aware() {
        let allowed = AuthScopes::parse(&["https://*.example.com:8443".into()]).unwrap();
        assert!(allowed.permits(&AuthScopes::parse(&["https://api.example.com:8443".into()]).unwrap()));
        assert!(allowed.permits(&AuthScopes::parse(&["https://*.api.example.com:8443".into()]).unwrap()));
        for origin in ["http://api.example.com:8443", "https://api.example.com", "https://evilexample.com:8443"] {
            assert!(!allowed.permits(&AuthScopes::parse(&[origin.into()]).unwrap()));
        }
        for invalid in ["*", "https://*", "ftp://x.test", "https://u:p@x.test", "https://x.test/path",
            "https://x.test/?secret=x", "https://x.test/#f", "https://*.127.0.0.1", "https://x.test\\"] {
            assert!(AuthScopes::parse(&[invalid.into()]).is_err(), "{invalid}");
        }
        assert_eq!(Origin::from_url("HTTPS://EXAMPLE.COM:443/a?q=x").unwrap().to_string(), "https://example.com");
        assert_eq!(Origin::from_url("http://[::1]:8080/a").unwrap().to_string(), "http://[::1]:8080");
    }

    #[test]
    fn shared_store_selection_revocation_and_late_binding() {
        clear();
        set_bearer_token_for_origins(&["https://*.example.com".into()], "wild").unwrap();
        set_user_credentials_for_origins(&["https://api.example.com".into()], "aladdin", "opensesame").unwrap();
        assert_eq!(resolve_header(KEY_USERCRED, "https://api.example.com/x").as_deref(), Some("Basic YWxhZGRpbjpvcGVuc2VzYW1l"));
        assert_eq!(resolve_header(KEY_TOKEN, "https://api.example.com/x"), None);
        assert_eq!(authorization_header_for("https://a.example.com/x").as_deref(), Some("Bearer wild"));
        clear_origin("https://a.example.com").unwrap();
        assert_eq!(authorization_header_for("https://a.example.com/x"), None);
        assert!(authorization_header_for("https://b.example.com/x").is_some());
        let before = authorization_header_for("https://api.example.com");
        assert!(set_bearer_token_for_origins(&["https://api.example.com".into(), "bad".into()], "new").is_err());
        assert_eq!(authorization_header_for("https://api.example.com"), before);
        assert!(set_bearer_token_for_origins(&["https://api.example.com".into()], "bad\r\nheader").is_err());
        let reference = credential_reference(KEY_USERCRED).unwrap();
        assert_eq!(reference, "auth:USERCRED");
        assert!(authorization_from_input(&reference, "https://api.example.com").is_some());
        assert_eq!(authorization_from_input(&reference, "https://elsewhere.test"), None);
        clear();
        assert_eq!(authorization_from_input(&reference, "https://api.example.com"), None);
    }

    #[test]
    fn base64_known_vectors() {
        for (input, output) in [("", ""), ("f", "Zg=="), ("fo", "Zm8="), ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"), ("Aladdin:open sesame", "QWxhZGRpbjpvcGVuIHNlc2FtZQ==")] {
            assert_eq!(base64_encode(input.as_bytes()), output);
        }
    }
}
