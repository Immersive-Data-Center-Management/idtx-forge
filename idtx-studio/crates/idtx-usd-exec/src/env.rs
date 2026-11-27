//! Generic Rust → `OpenExec` **environment-provider** registration.
//!
//! The exec plugin exports a C-ABI shim (`idtx_exec_env_register`, see
//! `cpp/env_provider_host.h`) that installs an `idtx::IEnvironmentProvider` under
//! a routing prefix into the plugin's `EnvironmentProviderRegistry`. A host that
//! wants an authored `inputs:key = "<prefix>:<name>"` to resolve implements
//! [`EnvProvider`] (or passes a closure) and calls [`register_provider_at`]; the
//! single trampoline here forwards each `Resolve()` to it. The C++ adapter owns
//! the `IEnvironmentProvider` vtable and all `std::string` traffic — only
//! `*const c_char` / length cross the boundary.
//!
//! Both built-in providers ([`crate::auth`] under `auth:` and [`crate::idcm`]
//! under `idcm:`) are thin wrappers over this module. Adding another is
//! deliberately small — implement `resolve(&self, key) -> Option<String>` and
//! register a prefix:
//!
//! ```no_run
//! use idtx_usd_exec::env;
//! # let dir = std::path::Path::new(".");
//! // A closure is a provider too (blanket impl below).
//! env::register_provider_at(dir, "greet", |key: &str| {
//!     (key == "HELLO").then(|| "world".to_owned())
//! });
//! ```
//!
//! This is also the seam a future plugin system registers through: a provider
//! loaded from a custom Rust or C++ plugin is, at this boundary, still just a
//! prefix plus a resolve callback — so the mechanism does not grow per provider.
//!
//! # How the registration reaches the plugin
//!
//! The registry is a singleton compiled *into* the plugin DLL, which the Rust
//! host never links. After the plugin is made resident (by [`crate::install`] /
//! [`crate::install_at`]) we resolve its exported `extern "C"`
//! `idtx_exec_env_register` symbol via `libloading` and call it with our
//! trampoline and the boxed provider as the context pointer.

use core::ffi::{c_char, c_void};
use std::panic::AssertUnwindSafe;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

/// A host-side environment provider: resolve a **de-prefixed** key to a value.
///
/// `key` is the authored input with the routing prefix already stripped by the
/// C++ registry (e.g. `TOKEN` for an authored `inputs:key = "auth:TOKEN"`).
/// Returning [`None`] reports the key as unresolved (empty output) to `OpenExec`.
///
/// Implementations must be thread-safe ([`Send`] + [`Sync`]): the exec
/// computation may invoke `Resolve` off the main thread. A panic is caught at
/// the FFI boundary and reported as unresolved, but a provider should still
/// prefer returning [`None`] over panicking.
pub trait EnvProvider: Send + Sync + 'static {
    /// Resolve `key` to a value, or [`None`] when this provider does not handle it.
    fn resolve(&self, key: &str) -> Option<String>;
}

/// Any `Fn(&str) -> Option<String>` is a provider, so a closure can be
/// registered directly without a named type.
impl<F> EnvProvider for F
where
    F: Fn(&str) -> Option<String> + Send + Sync + 'static,
{
    fn resolve(&self, key: &str) -> Option<String> {
        self(key)
    }
}

/// Sink the C++ adapter supplies to receive the resolved value.
type SinkFn = extern "C" fn(*mut c_void, *const c_char, usize);
/// Our resolve callback, matching `idtx_exec_resolve_fn` in `env_provider_host.h`.
type ResolveFn = extern "C" fn(*mut c_void, *const c_char, usize, SinkFn, *mut c_void) -> bool;
/// The plugin's exported registration entry point.
type RegisterFn = unsafe extern "C" fn(*const c_char, ResolveFn, *mut c_void);

/// The single host resolve callback handed to the plugin for every provider.
///
/// `ctx` is the `*mut Box<dyn EnvProvider>` leaked by [`register_provider_at`].
/// Reads the de-prefixed key, calls the provider, and on success writes the
/// value back through `sink`.
///
/// # Safety
///
/// Called by the plugin across the C ABI: `key`/`key_len` describe a valid byte
/// range for the call, `ctx` is the leaked boxed provider, and `sink`/`sink_ctx`
/// are the adapter's copy-back hook. No panic may unwind across the boundary, so
/// the provider call is wrapped in [`std::panic::catch_unwind`] and invalid
/// UTF-8 maps to unresolved rather than a slice panic.
extern "C" fn resolve_trampoline(
    ctx: *mut c_void,
    key: *const c_char,
    key_len: usize,
    sink: SinkFn,
    sink_ctx: *mut c_void,
) -> bool {
    if ctx.is_null() || key.is_null() {
        return false;
    }
    // SAFETY: `ctx` is the `*mut Box<dyn EnvProvider>` we leaked at registration;
    // it stays live for the process lifetime (never reclaimed after success).
    let provider: &dyn EnvProvider = unsafe { &**ctx.cast::<Box<dyn EnvProvider>>() };
    // SAFETY: the C-ABI contract guarantees `key`/`key_len` is a valid byte range
    // for the duration of the call.
    let bytes = unsafe { std::slice::from_raw_parts(key.cast::<u8>(), key_len) };
    let Ok(name) = std::str::from_utf8(bytes) else {
        return false;
    };
    // A provider callback must never unwind across the C boundary.
    match std::panic::catch_unwind(AssertUnwindSafe(|| provider.resolve(name))) {
        Ok(Some(value)) => {
            sink(sink_ctx, value.as_ptr().cast::<c_char>(), value.len());
            true
        }
        Ok(None) => false,
        Err(_) => {
            tracing::error!(key = name, "env provider panicked; reporting key unresolved");
            false
        }
    }
}

/// Platform file names of the exec plugin shared library (mirrors the names
/// `build.rs` emits and `crate::preload_plugin_global` dlopens).
fn plugin_lib_names() -> &'static [&'static str] {
    if cfg!(target_os = "windows") {
        &["idtx_usd_exec.dll"]
    } else if cfg!(any(target_os = "macos", target_os = "visionos")) {
        &["libidtx_usd_exec.dylib"]
    } else {
        &["libidtx_usd_exec.so"]
    }
}

/// Prefixes already registered this process, so repeat calls are no-ops (and we
/// never leak a second provider box for the same prefix).
fn registered() -> &'static Mutex<Vec<String>> {
    static REGISTERED: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    REGISTERED.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register `provider` under `prefix` into the resident exec plugin at `dir`.
///
/// Returns `true` on success (or if `prefix` was already registered). Idempotent
/// per prefix. Best-effort — a failure (plugin not yet resident, symbol missing,
/// `prefix` contains a NUL) is logged and returns `false`, so a host that never
/// opens an exec stage pays nothing and never crashes.
///
/// The provider and the library handle are intentionally leaked: the registered
/// C++ adapter calls back into this provider for the plugin's process lifetime.
pub fn register_provider_at(dir: &Path, prefix: &str, provider: impl EnvProvider) -> bool {
    {
        let mut done = registered().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if done.iter().any(|p| p == prefix) {
            return true;
        }
        // Reserve the prefix up front; on a failed load we remove it below so a
        // later retry (once the plugin is resident) can succeed.
        done.push(prefix.to_owned());
    }

    let Ok(c_prefix) = std::ffi::CString::new(prefix) else {
        tracing::warn!(prefix, "env provider prefix contains an interior NUL; not registered");
        unregister_bookkeeping(prefix);
        return false;
    };

    // The boxed provider is the context the trampoline reconstitutes. Leaked on
    // success; reclaimed if no plugin library could be opened.
    let boxed: *mut Box<dyn EnvProvider> =
        Box::into_raw(Box::new(Box::new(provider) as Box<dyn EnvProvider>));

    // Try the staged path first, then the bare soname (the Android app
    // native-lib-dir route, where the resident library resolves by name).
    let mut candidates: Vec<std::ffi::OsString> = Vec::new();
    for name in plugin_lib_names() {
        candidates.push(dir.join(name).into_os_string());
        candidates.push((*name).into());
    }

    for cand in &candidates {
        // SAFETY: opening an already-resident library; its initializers have
        // already run. We leak the handle (below) so the mapping persists.
        let Ok(lib) = (unsafe { libloading::Library::new(cand) }) else {
            continue;
        };
        // SAFETY: the symbol, if present, has exactly the C-ABI signature
        // declared in `env_provider_host.h`.
        let Ok(register) = (unsafe { lib.get::<RegisterFn>(b"idtx_exec_env_register\0") }) else {
            continue;
        };
        // SAFETY: `register` is the plugin's C-ABI entry; `resolve_trampoline` is
        // a valid callback and `boxed` is the leaked provider context it expects.
        unsafe {
            register(c_prefix.as_ptr(), resolve_trampoline, boxed.cast::<c_void>());
        }
        // Keep the plugin mapped for the process lifetime (the registered adapter
        // + our trampoline/provider must stay reachable). The plugin is already
        // held resident elsewhere, but leaking is the explicit guarantee.
        std::mem::forget(lib);
        tracing::debug!(prefix, "registered OpenExec environment provider");
        return true;
    }

    // No plugin library could be opened. Reclaim the provider box (nothing holds
    // it) and release the prefix reservation so a later retry can succeed.
    // SAFETY: `boxed` was just produced by `Box::into_raw` and never handed out.
    drop(unsafe { Box::from_raw(boxed) });
    unregister_bookkeeping(prefix);
    tracing::warn!(
        dir = %dir.display(),
        prefix,
        "could not register environment provider: idtx_exec_env_register not found \
         (is the OpenExec plugin resident?)"
    );
    false
}

/// Drop a prefix from the idempotency set (used when registration fails so a
/// retry is possible once the plugin is resident).
fn unregister_bookkeeping(prefix: &str) {
    let mut done = registered().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    done.retain(|p| p != prefix);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A closure resolves through the blanket [`EnvProvider`] impl, so providers
    /// can be registered without a named type.
    #[test]
    fn closure_is_a_provider() {
        let p = |key: &str| (key == "HELLO").then(|| "world".to_owned());
        assert_eq!(EnvProvider::resolve(&p, "HELLO").as_deref(), Some("world"));
        assert_eq!(EnvProvider::resolve(&p, "NOPE"), None);
    }

    /// Registering against a directory with no resident plugin fails gracefully
    /// (returns false) and — crucially — leaves the prefix free so a real
    /// `install()` can register it afterwards.
    #[test]
    fn missing_plugin_fails_without_poisoning_prefix() {
        let dir = std::path::Path::new("/idtx-nonexistent-plugin-dir");
        let prefix = "idtx-test-missing";
        assert!(!register_provider_at(dir, prefix, |_: &str| None));
        let done = registered().lock().unwrap();
        assert!(!done.iter().any(|p| p == prefix), "failed prefix must be released for retry");
    }
}
