//! `OpenUSD` `ArResolver` plugin for `http`/`https` asset URIs.
//!
//! Pairs a C++ resolver facade (`IdtxHttpArResolver.cpp`) with a Rust HTTP
//! fetcher: the Rust side arms the resolver with a fetch callback and an
//! on-disk cache directory, so a composed stage that references `http(s)://`
//! assets resolves them over the network. Fetches pass through the
//! `idtx-net-guard` SSRF gate ([`NetworkPolicy`]). Built and packaged as the
//! `idtx-http-resolver/` plugin directory staged beside the runtime.

use std::ffi::{CStr, CString};
use std::io::Read;
use std::os::raw::c_char;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

pub use idtx_net_guard::NetworkPolicy;
use idtx_net_guard::{locked_client, redact_url, validate_endpoint_with_timeout};

#[cfg(feature = "metrics")]
use tracing::info;
use tracing::{debug, warn};

// ── plugin dir baked in at compile time ──────────────────────────────────────

/// Directory containing the compiled resolver dylib and its `plugInfo.json`.
/// Set by build.rs via `cargo:rustc-env=IDTX_HTTP_RESOLVER_PLUGIN_DIR`.
///
/// Used as a fallback for development (`cargo run`). In a deployed `.app`
/// bundle, [`resolve_plugin_dir`] detects the bundle layout and returns the
/// bundle-relative path instead so the app is self-contained.
const PLUGIN_DIR: &str = env!("IDTX_HTTP_RESOLVER_PLUGIN_DIR");

/// Resolve the directory that holds the resolver library and its `plugInfo.json`
/// at runtime.
///
/// The plugin is packaged under an `idtx-http-resolver/` dir whose location
/// relative to the executable differs by bundle layout:
/// * Windows/Linux portable folder: exe at `<bundle>/<exe>`, plugin at
///   `<bundle>/idtx-http-resolver/` (the DLL/.so itself sits in the bundle root
///   beside the exe so the loader resolves the load-time import; the subfolder
///   holds only `plugInfo.json`, with `LibraryPath` emptied).
/// * macOS `.app`: exe at `<App>.app/Contents/MacOS/<exe>`, plugin at
///   `<App>.app/Contents/Frameworks/idtx-http-resolver/`.
/// * visionOS/iOS `.app` (flat): exe at `<App>.app/<exe>`, plugin at
///   `<App>.app/Frameworks/idtx-http-resolver/`.
///
/// We probe `idtx-http-resolver/` both directly and under `Frameworks/` against
/// the executable's own dir and a couple of ancestors, returning the first that
/// carries a `plugInfo.json` — this makes the bundle self-contained with no
/// build-machine paths, on any layout. Otherwise we fall back to `PLUGIN_DIR`,
/// the compile-time `OUT_DIR` baked in for `cargo run` during development.
fn resolve_plugin_dir() -> std::path::PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        // exe dir and up to two ancestors (macOS `Contents/MacOS` → `Contents`,
        // then the `.app` root) each may hold the plugin dir — directly (desktop
        // portable folder) or under a sibling `Frameworks/` (the `.app` layouts).
        let mut dir = exe.parent();
        for _ in 0..3 {
            let Some(d) = dir else { break };
            for candidate in [
                d.join("idtx-http-resolver"),
                d.join("Frameworks").join("idtx-http-resolver"),
            ] {
                if candidate.join("plugInfo.json").exists() {
                    return candidate;
                }
            }
            dir = d.parent();
        }
    }
    std::path::PathBuf::from(PLUGIN_DIR)
}

// ── FFI declaration ───────────────────────────────────────────────────────────

type IdtxFetchFn = unsafe extern "C" fn(*const c_char, *const c_char) -> bool;

unsafe extern "C" {
    /// Installed in `IdtxHttpArResolver.cpp`. Arms the resolver with the Rust
    /// fetch callback, sets the on-disk cache directory, and registers the
    /// plugin's `plugInfo.json` (in `plugin_dir`) with the `OpenUSD` plugin
    /// registry at runtime. Must be called before the first USD stage that
    /// references http/https URIs is opened.
    fn idtx_http_resolver_install(
        fetch_fn: Option<IdtxFetchFn>,
        cache_dir: *const c_char,
        plugin_dir: *const c_char,
    );
}

// ── bearer token source ─────────────────────────────────────────────────────────

// All consumers read the same origin-scoped idtx-auth store. Credentials are
// selected using the full destination URL, including scheme and effective port.

/// Resolve against the complete destination URL; there is no unscoped fallback.
fn auth_header_for(url: &str) -> Option<String> {
    idtx_auth::authorization_header_for(url)
}

/// Origin of the most recent `401 Unauthorized` fetch, recorded so the host
/// application can drive an `idtx_remote::AuthProvider` refresh + retry.
static LAST_UNAUTHORIZED: OnceLock<Mutex<Option<String>>> = OnceLock::new();

fn last_unauthorized_slot() -> &'static Mutex<Option<String>> {
    LAST_UNAUTHORIZED.get_or_init(|| Mutex::new(None))
}

/// Every fetch snapshots this shared policy. Unconfigured and poisoned state
/// deny all egress. Editors explicitly select the public-internet posture.
static NETWORK_POLICY: OnceLock<Mutex<NetworkPolicy>> = OnceLock::new();

fn policy_slot() -> &'static Mutex<NetworkPolicy> {
    NETWORK_POLICY.get_or_init(|| Mutex::new(NetworkPolicy::denied()))
}

fn network_policy() -> NetworkPolicy {
    policy_slot().lock().map_or_else(|_| NetworkPolicy::denied(), |p| p.clone())
}

/// Replace the process-wide asset egress policy before opening a stage. Network
/// and credential grants are independent; this operation grants no credentials.
pub fn set_network_policy(policy: NetworkPolicy) {
    let slot = policy_slot();
    *slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = policy;
    slot.clear_poison();
}

/// Load configuration with egress denied throughout parsing. An error or panic
/// leaves the resolver denied instead of retaining a previous bundle's grants.
/// Shared by desktop, Android and the winit-free visionOS bootstrap.
pub fn configure_network<T, E>(
    load: impl FnOnce() -> Result<T, E>,
    policy: impl FnOnce(&T) -> NetworkPolicy,
) -> Result<T, E> {
    set_network_policy(NetworkPolicy::denied());
    let config = load()?;
    set_network_policy(policy(&config));
    Ok(config)
}

/// Compatibility setter: named public hosts only, with the default asset cap.
/// Private destinations require [`set_network_policy`] with explicit grants.
pub fn set_allowed_hosts(hosts: &[String]) {
    set_network_policy(NetworkPolicy::new(true, hosts.to_vec())
        .with_max_response_mb(idtx_net_guard::DEFAULT_MAX_ASSET_MB));
}

/// Grant the current egress policy loopback/RFC1918 access for additional host
/// patterns, keeping every grant already configured (project `private_hosts`,
/// earlier server grants). Use this so a stage opened from an authenticated
/// server — including a `localhost`/LAN dev server, which the public-internet
/// posture otherwise rejects as a blocked private address — can be fetched.
/// Grants no credentials and does not touch the master switch or the host gate.
pub fn add_private_hosts(hosts: &[String]) {
    let slot = policy_slot();
    slot.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .merge_private_hosts(hosts.iter().cloned());
    slot.clear_poison();
}

/// Take (and clear) the origin of the most recent `401 Unauthorized` fetch, if
/// any. The host application polls this after a failed stage open to decide
/// whether to refresh credentials (via an `idtx_remote::AuthProvider`) and retry.
///
/// # Panics
/// Panics if the internal mutex has been poisoned.
#[must_use]
pub fn take_unauthorized_origin() -> Option<String> {
    last_unauthorized_slot()
        .lock()
        .expect("unauthorized-host lock poisoned")
        .take()
}

// ── on-disk cache force-refresh ───────────────────────────────────────────────

unsafe extern "C" {
    /// Installed in `IdtxHttpArResolver.cpp`. Arms/disarms force-refresh: while
    /// armed, the resolver re-fetches every asset it opens even if cached,
    /// overwriting the cache file in place.
    fn idtx_http_resolver_set_force_refresh(on: bool);

    /// Installed in `IdtxHttpArResolver.cpp`. Arms/disarms packaging mode: while
    /// armed, the resolver reports every `http(s)` URI as unresolvable so the
    /// USDZ dependency localizer leaves remote references authored (external)
    /// rather than fetching + embedding them.
    fn idtx_http_resolver_set_packaging_mode(on: bool);
}

/// Arms or disarms force-refresh in the resolver.
///
/// While armed, the next stage open re-downloads **every** asset the resolver
/// touches — the root layer plus all referenced sublayers, references, and
/// textures — overwriting the cached copies in place, instead of serving the
/// cache. Normal opens leave this off and stay offline.
///
/// The flag is process-global, so arm it immediately before a single stage
/// open and disarm it immediately after (see the shell's Redownload path).
/// Overlapping opens on other threads during the armed window would also
/// refresh; Redownload is a user-driven, one-at-a-time action, so this does
/// not arise in practice.
pub fn set_force_refresh(on: bool) {
    // SAFETY: the C++ setter only stores into an atomic; no aliasing or
    // lifetime concerns. Safe to call from any thread at any time.
    unsafe { idtx_http_resolver_set_force_refresh(on) };
}

/// Arms or disarms packaging mode in the resolver.
///
/// While armed, the resolver reports every `http(s)` URI as **unresolvable**.
/// `OpenUSD`'s dependency localization (`UsdUtilsCreateNewUsdzPackage`) then
/// leaves remote references **authored** in the produced package instead of
/// fetching the asset and rewriting the URL to a package-internal path. This is
/// how a cook keeps remote payloads/references external (see
/// [`idtx_usd::package_stage_keeping_remote`]).
///
/// The flag is process-global, so arm it immediately before a single package
/// call and disarm it immediately after (the cook does this under the exclusive
/// stage-lifecycle lock, so no concurrent read-projection is affected).
pub fn set_packaging_mode(on: bool) {
    // SAFETY: the C++ setter only stores into an atomic; no aliasing or
    // lifetime concerns. Safe to call from any thread at any time.
    unsafe { idtx_http_resolver_set_packaging_mode(on) };
}

// ── fetch callback ────────────────────────────────────────────────────────────

const FETCH_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, thiserror::Error)]
enum FetchError {
    #[error("endpoint refused: {0}")]
    Endpoint(idtx_net_guard::EndpointRejection),
    #[error("HTTP {0}")]
    HttpStatus(u16),
    #[error("HTTP request failed: {0}")]
    Request(reqwest::Error),
    #[error("response exceeds {limit} bytes")]
    TooLarge { limit: usize },
    // A reqwest-backed io::Error may itself contain the full URL. Keep only the
    // I/O classification when reporting streamed read failures.
    #[error("response read failed: {0:?}")]
    Body(std::io::ErrorKind),
    #[error("cache write failed: {0}")]
    Cache(#[from] std::io::Error),
    #[error("asset fetch deadline expired")]
    TimedOut,
}

/// Validate, pin and stream into a unique temporary file in the destination's
/// directory. Nothing is published until a complete, within-cap body arrives.
fn fetch_to_cache(url: &str, dest: &Path, policy: &NetworkPolicy, timeout: Duration) -> Result<u64, FetchError> {
    let started = Instant::now();
    let validated = validate_endpoint_with_timeout(url, policy, timeout).map_err(FetchError::Endpoint)?;
    let remaining = timeout.checked_sub(started.elapsed()).filter(|t| !t.is_zero()).ok_or(FetchError::TimedOut)?;
    let client = locked_client(&validated, remaining).map_err(|e| FetchError::Request(e.without_url()))?;
    let remaining = timeout.checked_sub(started.elapsed()).filter(|t| !t.is_zero()).ok_or(FetchError::TimedOut)?;
    // Set the request timeout too: reqwest's blocking-client timeout alone is
    // per read, so a slow-drip body could otherwise reset it indefinitely.
    let send = |remaining| {
        let credential = auth_header_for(validated.url());
        let mut request = client.get(validated.url()).timeout(remaining)
            .header(reqwest::header::USER_AGENT, concat!("idtx-studio/", env!("CARGO_PKG_VERSION")));
        if let Some(value) = &credential { request = request.header(reqwest::header::AUTHORIZATION, value); }
        request.send().map(|response| (response, credential)).map_err(|e| FetchError::Request(e.without_url()))
    };
    let (mut response, credential) = send(remaining)?;
    if response.status().as_u16() == 401 {
        let remaining = timeout.saturating_sub(started.elapsed());
        if idtx_auth::refresh_after_unauthorized(validated.url(), credential.as_deref(), remaining) {
            let remaining = timeout.checked_sub(started.elapsed()).filter(|t| !t.is_zero()).ok_or(FetchError::TimedOut)?;
            response = send(remaining)?.0;
        }
    }
    if !response.status().is_success() {
        let status = response.status().as_u16();
        if status == 401 && let Ok(origin) = idtx_auth::Origin::from_url(validated.url()) {
            *last_unauthorized_slot().lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(origin.to_string());
        }
        return Err(FetchError::HttpStatus(status));
    }
    let limit = policy.max_response_bytes();
    if response.content_length().is_some_and(|length| length > limit as u64) {
        return Err(FetchError::TooLarge { limit });
    }
    let parent = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    let mut body = response.take((limit as u64).saturating_add(1));
    let bytes = std::io::copy(&mut body, temporary.as_file_mut()).map_err(|e| FetchError::Body(e.kind()))?;
    if bytes > limit as u64 { return Err(FetchError::TooLarge { limit }); }
    if started.elapsed() >= timeout { return Err(FetchError::TimedOut); }
    temporary.persist(dest).map_err(|e| FetchError::Cache(e.error))?;
    Ok(bytes)
}

/// Called by `IdtxHttpArResolver::_OpenAsset` on the thread that triggered
/// the USD asset open. Downloads `url` to `dest_path`, returning true on
/// success. The function is registered via `idtx_http_resolver_install()`.
unsafe extern "C" fn rust_fetch(url_ptr: *const c_char, dest_ptr: *const c_char) -> bool {
    if url_ptr.is_null() || dest_ptr.is_null() { return false; }
    // SAFETY: both pointers come from C++ string data in the resolver;
    // they are non-null, null-terminated, and valid for the duration of
    // this call (which returns before the C++ stack frame unwinds).
    let url = unsafe { CStr::from_ptr(url_ptr) }
        .to_str()
        .unwrap_or_default();
    let dest = unsafe { CStr::from_ptr(dest_ptr) }
        .to_str()
        .unwrap_or_default();

    if url.is_empty() || dest.is_empty() {
        return false;
    }

    #[cfg(feature = "metrics")]
    let t = std::time::Instant::now();
    let safe_url = redact_url(url);
    debug!(url = %safe_url, "resolver fetch requested");
    match fetch_to_cache(url, Path::new(dest), &network_policy(), FETCH_TIMEOUT) {
        Ok(bytes) => {
            #[cfg(feature = "metrics")]
            info!(url = %safe_url, bytes, ms = t.elapsed().as_millis(), "asset fetched");
            debug!(url = %safe_url, bytes, "cached remote asset");
            true
        }
        Err(error) => {
            warn!(url = %safe_url, %error, "resolver fetch failed");
            false
        }
    }
}

// ── public install API ────────────────────────────────────────────────────────

/// Registers the `IdtxHttpArResolver` with the `OpenUSD` plugin system and arms
/// the Rust fetch callback.
///
/// **Must be called once, before any USD API (including `TfToken`) is invoked
/// in the process.** `PXR_PLUGINPATH_NAME` must be set before the `Ar`
/// subsystem initialises (which happens lazily on first use).
///
/// `cache_dir` is the directory where downloaded assets are stored.
/// It is created automatically if it does not exist.
pub fn install(cache_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    install_with_plugin_dir(cache_dir, &resolve_plugin_dir())
}

/// Like [`install`], but registers an explicit `plugin_dir` (the directory
/// holding the resolver's `plugInfo.json`) instead of the auto-discovered one.
/// Exposed for tests and packaging to validate alternative bundle layouts
/// (e.g. a staged plugInfo with an emptied `LibraryPath`).
#[doc(hidden)]
pub fn install_with_plugin_dir(
    cache_dir: &Path,
    plugin_dir: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // 1. Ensure the cache directory exists.
    std::fs::create_dir_all(cache_dir)?;

    // 2. Prepend our plugin dir to PXR_PLUGINPATH_NAME. NOTE: this alone does
    //    NOT make OpenUSD discover the plugin — Plug reads PXR_PLUGINPATH_NAME
    //    in an ARCH_CONSTRUCTOR (plug/initConfig.cpp) that runs at library-load
    //    time, before main(), so a runtime mutation here is too late. We still
    //    set it for documentation and for any child processes that inherit the
    //    environment; the authoritative registration happens in step 3 via the
    //    C++ `PlugRegistry::RegisterPlugins()` call.
    let current = std::env::var("PXR_PLUGINPATH_NAME").unwrap_or_default();
    let new_val = if current.is_empty() {
        plugin_dir.to_string_lossy().into_owned()
    } else {
        format!("{}:{}", plugin_dir.display(), current)
    };
    // SAFETY: no other threads are running USD code at this point (documented
    // precondition: install() must be the first USD-related call in main()).
    unsafe { std::env::set_var("PXR_PLUGINPATH_NAME", &new_val) };

    // 3. Arm the C++ resolver with the Rust fetch callback and cache dir, and
    //    register the plugin directory with PlugRegistry at runtime so the
    //    IdtxHttpArResolver type is associated with its plugInfo.json before Ar
    //    enumerates URI resolvers on first stage open.
    let cache_cstr = CString::new(
        cache_dir
            .to_str()
            .ok_or("cache_dir contains non-UTF-8 bytes")?,
    )?;
    let plugin_cstr = CString::new(
        plugin_dir
            .to_str()
            .ok_or("plugin_dir contains non-UTF-8 bytes")?,
    )?;
    unsafe {
        idtx_http_resolver_install(Some(rust_fetch), cache_cstr.as_ptr(), plugin_cstr.as_ptr());
    }

    debug!(
        plugin_dir = %plugin_dir.display(),
        cache_dir  = %cache_dir.display(),
        "IdtxHttpArResolver installed"
    );
    Ok(())
}

#[cfg(test)]
static TEST_POLICY_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
mod fetch_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poisoned_policy_fails_closed_until_explicitly_replaced() {
        let _lock = TEST_POLICY_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        set_network_policy(NetworkPolicy::public_internet());
        let _ = std::panic::catch_unwind(|| {
            let _policy = policy_slot().lock().unwrap();
            panic!("synthetic policy panic");
        });
        assert!(network_policy().is_denied());
        set_network_policy(NetworkPolicy::denied());
        assert!(!policy_slot().is_poisoned());
    }

    /// One serialized test of the shared origin-scoped lookup and logout.
    #[test]
    fn auth_header_requires_an_authorized_origin() {
        idtx_auth::clear();
        idtx_auth::set_bearer_token_for_origins(&["https://assets.example.com".to_owned()], "host-tok").unwrap();
        assert_eq!(
            auth_header_for("https://assets.example.com/a").as_deref(),
            Some("Bearer host-tok")
        );
        assert_eq!(auth_header_for("https://other.example.com/a"), None);
        assert_eq!(auth_header_for("http://assets.example.com/a"), None);
        assert_eq!(auth_header_for("https://assets.example.com:8443/a"), None);
        assert_eq!(auth_header_for("assets.example.com"), None);
        idtx_auth::clear();
        assert_eq!(auth_header_for("https://assets.example.com/a"), None);
    }

    /// `take_unauthorized_origin` is take-and-clear. Touches the process-global
    /// unauthorized slot (independent of the `idtx_auth` bucket above).
    #[test]
    fn unauthorized_take_is_take_and_clear() {
        assert_eq!(take_unauthorized_origin(), None);
        *last_unauthorized_slot().lock().expect("unauthorized lock") =
            Some("https://locked.example.com:8443".to_string());
        assert_eq!(
            take_unauthorized_origin().as_deref(),
            Some("https://locked.example.com:8443")
        );
        assert_eq!(take_unauthorized_origin(), None);
    }
}
