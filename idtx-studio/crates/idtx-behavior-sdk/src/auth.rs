//! Auth-bucket writes (`auth_write` capability).
//!
//! A behavior granted `auth_write` writes only within the host's explicit
//! `[network] auth_origins` group. `set_token` / `set_user_credentials` use the
//! whole group; `_for_host(s)` selects a permitted subset. Selectors may be full
//! origins (`http://api.example:8080`, `https://*.assets.example`) or legacy bare
//! authorities (HTTPS is assumed). Empty/unconfigured groups deny writes.
//!
//! USD assets, behavior HTTP and `OpenExec` all resolve the same scoped credentials
//! against the actual request origin. A network allowlist alone grants no token.
//!
//! The bucket is **write-only** from a behavior — there is deliberately no
//! read-back import, so one sandboxed behavior cannot exfiltrate credentials
//! another stored.
//!
//! On wasm these route to the capability-gated `idtx_auth_*` host imports
//! (absent — and therefore link-failing — without the grant); natively they are
//! inert `Ok(())` so a behavior using them stays unit-testable without a host.

use crate::result::{check, Result};

/// Non-secret credential generation. Capture when starting login; discard a
/// late result if logout or a newer login changed this generation.
pub fn revision() -> u64 {
    #[cfg(target_arch = "wasm32")]
    { unsafe { crate::__rt::idtx_auth_revision() } }
    #[cfg(not(target_arch = "wasm32"))]
    { 0 }
}

/// Store a raw Bearer token for the host's authorized origin group. Replaces
/// credentials in that group; fails with CapabilityDenied when it is empty.
pub fn set_token(token: &str) -> Result<()> {
    #[cfg(target_arch = "wasm32")]
    let code = crate::abi::ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_auth_set_token(token.as_ptr(), u32::try_from(token.len()).unwrap_or(u32::MAX))
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        let _ = token;
        crate::abi::ErrorCode::Ok
    };
    check(code)
}

/// Store HTTP Basic credentials, replacing any previously stored credentials.
/// The host encodes `base64(user:password)` when resolving `auth:USERCRED`.
pub fn set_user_credentials(user: &str, password: &str) -> Result<()> {
    #[cfg(target_arch = "wasm32")]
    let code = crate::abi::ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_auth_set_usercred(
            user.as_ptr(),
            u32::try_from(user.len()).unwrap_or(u32::MAX),
            password.as_ptr(),
            u32::try_from(password.len()).unwrap_or(u32::MAX),
        )
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        let _ = (user, password);
        crate::abi::ErrorCode::Ok
    };
    check(code)
}

/// Revoke the host-authorized origin group; future lookups resolve to nothing.
pub fn clear() -> Result<()> {
    #[cfg(target_arch = "wasm32")]
    let code = crate::abi::ErrorCode::from_u32(unsafe { crate::__rt::idtx_auth_clear() });
    #[cfg(not(target_arch = "wasm32"))]
    let code = crate::abi::ErrorCode::Ok;
    check(code)
}

/// Store a Bearer token for an origin selector within the host's authorized
/// group. Full HTTP(S) origins include the effective port; a bare authority uses
/// HTTPS. This same grant serves USD, behavior HTTP, and `OpenExec` requests.
pub fn set_token_for_host(host: &str, token: &str) -> Result<()> {
    #[cfg(target_arch = "wasm32")]
    let code = crate::abi::ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_auth_set_token_for_host(
            host.as_ptr(),
            u32::try_from(host.len()).unwrap_or(u32::MAX),
            token.as_ptr(),
            u32::try_from(token.len()).unwrap_or(u32::MAX),
        )
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        let _ = (host, token);
        crate::abi::ErrorCode::Ok
    };
    check(code)
}

/// Share one Bearer token across an explicit set of `hosts` — e.g. a login token
/// an IDTX-Core host mints that an AAS host on a different domain also accepts.
/// The token reaches every listed host but no other allowlisted host. Registers
/// each host in turn; the first failure stops and is returned.
pub fn set_token_for_hosts(hosts: &[&str], token: &str) -> Result<()> {
    for host in hosts {
        set_token_for_host(host, token)?;
    }
    Ok(())
}

/// Store HTTP Basic credentials **scoped to `host`** for `net` egress to that
/// host only (see [`set_token_for_host`]).
pub fn set_user_credentials_for_host(host: &str, user: &str, password: &str) -> Result<()> {
    #[cfg(target_arch = "wasm32")]
    let code = crate::abi::ErrorCode::from_u32(unsafe {
        crate::__rt::idtx_auth_set_usercred_for_host(
            host.as_ptr(),
            u32::try_from(host.len()).unwrap_or(u32::MAX),
            user.as_ptr(),
            u32::try_from(user.len()).unwrap_or(u32::MAX),
            password.as_ptr(),
            u32::try_from(password.len()).unwrap_or(u32::MAX),
        )
    });
    #[cfg(not(target_arch = "wasm32"))]
    let code = {
        let _ = (host, user, password);
        crate::abi::ErrorCode::Ok
    };
    check(code)
}

/// Share one HTTP Basic credential across an explicit set of `hosts` (see
/// [`set_token_for_hosts`] for the sharing rationale).
pub fn set_user_credentials_for_hosts(hosts: &[&str], user: &str, password: &str) -> Result<()> {
    for host in hosts {
        set_user_credentials_for_host(host, user, password)?;
    }
    Ok(())
}
