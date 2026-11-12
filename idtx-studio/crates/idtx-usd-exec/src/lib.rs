//! Coded `OpenUSD` **`OpenExec`** dataflow plugin for IDTX.
//!
//! This crate ships a compiled `OpenUSD` plugin (`idtx_usd_exec.dll` /
//! `libidtx_usd_exec.{dylib,so}`, built by `build.rs` from `cpp/`) that registers
//! a set of `OpenExec` dataflow schemas and their computations:
//!
//! * **Datasources** — [`REST_DATASOURCE_TYPE`] / [`MOCK_DATASOURCE_TYPE`] (both
//!   `IsA` [`DATASOURCE_TYPE`]) whose [`ATTR_OUTPUTS_DATA`] carries a JSON string
//!   fetched from an external source (a REST endpoint, or random mock values).
//! * **Compute nodes** — [`COMPUTE_VALUE_FROM_JSON_TYPE`],
//!   [`COMPUTE_ENVIRONMENT_TYPE`],
//!   [`COMPUTE_VAR_REPLACEMENTS_TYPE`], each registering an `OpenExec` computation
//!   (`EXEC_REGISTER_COMPUTATIONS_FOR_SCHEMA` in `cpp/computation_*.cpp`).
//!
//! The dataflow is: an external source authors JSON onto a datasource's
//! `outputs:data`, which invalidates the `OpenExec` compute chain; the recomputed
//! value is authored onto the consuming attribute in the stage's **session
//! layer**, where idtx-studio's existing change-notice → hydrate path renders it.
//! That host-side wiring lives in `idtx-usd-sys` (the exec bridge) and
//! `idtx-studio-session` (`tick_exec`); this crate provides only the USD plugin
//! and its registration.
//!
//! # Usage
//!
//! Call [`install`] once, **before** opening any stage that carries these
//! schemas, so the types + computations are in the registry when the stage is
//! composed:
//!
//! ```no_run
//! idtx_usd_exec::install().expect("register IDTX OpenExec plugin");
//! ```

use std::path::PathBuf;

pub use idtx_usd::StageError;

pub mod auth;
pub mod env;
pub mod idcm;

// ── schema USD type names (schemaIdentifier / alias; what `type_name` compares) ─

/// Abstract base `IsA` type of every datasource (`outputs:data` JSON carrier).
pub const DATASOURCE_TYPE: &str = "Datasource";
/// Concrete `IsA` type driven by a REST endpoint (host-polled, see `tick_exec`).
pub const REST_DATASOURCE_TYPE: &str = "RestDatasource";
/// Concrete `IsA` type emitting random mock JSON — the network-free test source.
pub const MOCK_DATASOURCE_TYPE: &str = "MockDatasource_RandomFloat";

/// Compute node extracting a field from a JSON string ([`ATTR_INPUTS_JSON_DATA`]
/// at [`ATTR_INPUTS_JSON_PATH`], typed by [`ATTR_JSON_VALUE_TYPE`]).
pub const COMPUTE_VALUE_FROM_JSON_TYPE: &str = "Compute_ValueFromJson";
/// Compute node extracting a field from an **AAS** (Asset Administration Shell)
/// JSON string. Same attributes as [`COMPUTE_VALUE_FROM_JSON_TYPE`], but the
/// [`ATTR_INPUTS_JSON_PATH`] navigates by AAS `idShort` values (descending
/// `submodelElements` / `value` containers and OperationVariable wrappers),
/// which a plain RFC-6901 JSON pointer cannot express.
pub const COMPUTE_VALUE_FROM_AAS_JSON_TYPE: &str = "Compute_ValueFromAASJson";
/// Compute node resolving [`ATTR_INPUTS_KEY`] via a host environment provider.
pub const COMPUTE_ENVIRONMENT_TYPE: &str = "Compute_Environment";
/// Compute node replacing `${type:name}` tokens in [`ATTR_INPUTS_TEMPLATE`].
pub const COMPUTE_VAR_REPLACEMENTS_TYPE: &str = "Compute_VarReplacements";

/// Every concrete schema type this plugin registers, for tests / introspection.
pub const ALL_SCHEMA_TYPES: [&str; 6] = [
    REST_DATASOURCE_TYPE,
    MOCK_DATASOURCE_TYPE,
    COMPUTE_VALUE_FROM_JSON_TYPE,
    COMPUTE_VALUE_FROM_AAS_JSON_TYPE,
    COMPUTE_ENVIRONMENT_TYPE,
    COMPUTE_VAR_REPLACEMENTS_TYPE,
];

/// Every schema type carrying a registered `OpenExec` computation — mirrors the
/// `"Exec" > "Schemas"` discovery block in `resources/plugInfo.json`.
pub const COMPUTE_SCHEMA_TYPES: [&str; 4] = [
    COMPUTE_VALUE_FROM_JSON_TYPE,
    COMPUTE_VALUE_FROM_AAS_JSON_TYPE,
    COMPUTE_ENVIRONMENT_TYPE,
    COMPUTE_VAR_REPLACEMENTS_TYPE,
];

// ── attribute / token identifiers ────────────────────────────────────────────

/// `string` — JSON output of any [`DATASOURCE_TYPE`]; the host authors the
/// external payload here to drive the compute chain.
pub const ATTR_OUTPUTS_DATA: &str = "outputs:data";

// RestDatasource config (read by the host REST poller):
/// `string` — base URL of the REST endpoint.
pub const ATTR_ENDPOINT: &str = "endpoint";
/// `string` — url-encoded query appended after the endpoint path.
pub const ATTR_QUERY: &str = "query";
/// `token` — HTTP method, `GET` (default) or `POST`.
pub const ATTR_METHOD: &str = "method";
/// `string` — `Authorization` header value (e.g. `Bearer <TOKEN>`).
pub const ATTR_AUTHORIZATION: &str = "authorization";
/// `string` — JSON body sent when [`ATTR_METHOD`] is `POST`.
pub const ATTR_JSON_BODY: &str = "jsonBody";
/// `float` — poll interval in seconds (schema default `1.0`).
pub const ATTR_INTERVAL: &str = "interval";

// Compute_ValueFromJson:
/// `string` — JSON input the value is extracted from.
pub const ATTR_INPUTS_JSON_DATA: &str = "inputs:jsonData";
/// `string` — JSON-pointer path of the field to extract.
pub const ATTR_INPUTS_JSON_PATH: &str = "jsonPath";
/// `token` — extracted value type: `none`/`float`/`double`/`string`.
pub const ATTR_JSON_VALUE_TYPE: &str = "jsonValueType";

// Compute_Environment / Compute_VarReplacements:
/// `string` — provider key resolved by `Compute_Environment` (e.g. `env:PATH`).
pub const ATTR_INPUTS_KEY: &str = "inputs:key";
/// `string` — resolved value output of `Compute_Environment`.
pub const ATTR_OUTPUTS_VALUE: &str = "outputs:value";
/// `string` — template with `${type:name}` tokens for `Compute_VarReplacements`.
pub const ATTR_INPUTS_TEMPLATE: &str = "inputs:template";

/// Allowed values of [`ATTR_METHOD`], in `allowedTokens` order.
pub const METHOD_TOKENS: [&str; 2] = ["GET", "POST"];
/// Allowed values of [`ATTR_JSON_VALUE_TYPE`], in `allowedTokens` order.
pub const JSON_VALUE_TYPE_TOKENS: [&str; 4] = ["none", "float", "double", "string"];

// ── plugin directory resolution + registration ──────────────────────────────

/// Directory holding the plugin DLL + `plugInfo.json` + `generatedSchema.usda`,
/// baked in at compile time by `build.rs` (points into `OUT_DIR`). Used for
/// `cargo run` and tests; a deployed bundle is handled by [`plugin_dir`].
pub const BAKED_PLUGIN_DIR: &str = env!("IDTX_USD_EXEC_PLUGIN_DIR");

/// Resolve the directory that holds the plugin `plugInfo.json` at runtime.
///
/// In a macOS `.app` bundle the resources are packaged at
/// `<App>.app/Contents/Resources/idtx-usd-exec/`; if present we return it so the
/// bundle is self-contained, else we fall back to [`BAKED_PLUGIN_DIR`]. Mirrors
/// `idtx_usd_extension::plugin_dir`.
#[must_use]
pub fn plugin_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(macos_dir) = exe.parent() {
            if macos_dir.file_name().is_some_and(|n| n == "MacOS") {
                if let Some(contents) = macos_dir.parent() {
                    let bundled = contents.join("Resources").join("idtx-usd-exec");
                    if bundled.join("plugInfo.json").exists() {
                        return bundled;
                    }
                }
            }
        }
    }
    PathBuf::from(BAKED_PLUGIN_DIR)
}

/// Register the IDTX `OpenExec` plugin with the `OpenUSD` plugin registry.
///
/// **Call once, before opening any stage that uses these schemas.** Goes through
/// [`idtx_usd::register_plugin_path`] → `PlugRegistry::RegisterPlugins`, the only
/// reliable runtime path (`PXR_PLUGINPATH_NAME` is read at library-load time, so
/// setting it post-start is too late). Idempotent: after the first success,
/// further calls are no-ops.
///
/// # Errors
/// Returns [`StageError`] if the plugin directory path is non-UTF-8 or `OpenUSD`
/// rejects the registration.
pub fn install() -> Result<(), StageError> {
    register_from(&plugin_dir())
}

/// Register the IDTX `OpenExec` plugin from an **explicit** staged directory (a
/// cooked payload's plugin dir) rather than the compile-time [`BAKED_PLUGIN_DIR`]
/// — which does not exist on a packaged device. Shares [`install`]'s idempotency.
///
/// # Errors
/// Returns [`StageError`] if the path is non-UTF-8 or `OpenUSD` rejects it.
pub fn install_at(dir: impl AsRef<std::path::Path>) -> Result<(), StageError> {
    register_from(dir.as_ref())
}

/// Shared idempotent `PlugRegistry::RegisterPlugins(dir)` body for [`install`] /
/// [`install_at`].
fn register_from(dir: &std::path::Path) -> Result<(), StageError> {
    use std::sync::atomic::{AtomicBool, Ordering};
    static REGISTERED: AtomicBool = AtomicBool::new(false);
    if REGISTERED.load(Ordering::Acquire) {
        return Ok(());
    }

    #[cfg(feature = "metrics")]
    let start = std::time::Instant::now();

    // Standalone (Android / visionOS): the plugin is a link-time `DT_NEEDED` of the
    // runtime cdylib (emitted by build.rs), so the dynamic linker already loaded it
    // in the runtime's dependency closure — the same scope as `libusd_ms.so` — and
    // the weak C++ `type_info`/`VtValue` symbols collapse to one copy. (A late
    // `dlopen`, even `RTLD_GLOBAL` from the app's native-lib dir, does NOT merge
    // into that closure on bionic/Apple libc++, so the types would split and
    // OpenExec aborts: "holding string, tried to use as string".) No runtime work
    // is needed here for that path — the linker already recorded the dependency.

    // Desktop Unix (macOS / Linux): no `DT_NEEDED`; promote `usd_ms` + load the
    // plugin `RTLD_GLOBAL` so its types bind to `usd_ms`'s. Desktop Windows uses
    // PlugRegistry's own load (type_info compared by name there), so no preload.
    #[cfg(all(unix, not(any(target_os = "android", target_os = "visionos"))))]
    preload_plugin_global(dir);

    idtx_usd::register_plugin_path(dir)?;
    REGISTERED.store(true, Ordering::Release);

    // The plugin is now resident, so its exported `EnvironmentProviderRegistry`
    // is reachable: install the Rust-side default providers (idempotent,
    // best-effort). Every host that registers the plugin — runtime desktop/XR,
    // Android, visionOS, and the editor's Compute Graph preview — gets them for
    // free from this single call site. New providers should be added here (and
    // mirror the sibling `idtx-flow-idcm`'s `register_types.cpp`).
    auth::register_default_auth_provider_at(dir); // "auth:" — late-bound credential references
    idcm::register_default_idcm_provider_at(dir); // "idcm:" — dynamic built-in variables

    #[cfg(feature = "metrics")]
    tracing::info!(
        plugin_dir = %dir.display(),
        ms = start.elapsed().as_secs_f64() * 1e3,
        "registered IDTX OpenExec plugin"
    );
    #[cfg(not(feature = "metrics"))]
    tracing::debug!(plugin_dir = %dir.display(), "registered IDTX OpenExec plugin");

    Ok(())
}

/// Load the plugin shared library from `dir` with `RTLD_NOW | RTLD_GLOBAL` so its
/// symbols (notably the weak `std::string`/`VtValue` `type_info`s) join the global
/// scope and resolve to `usd_ms`'s already-loaded copies. Idempotent at the OS
/// level (a second `dlopen` of a resident library returns the same handle; the
/// `RTLD_GLOBAL` promotion is sticky), and the handle is intentionally leaked —
/// the plugin must stay resident for the process lifetime. Best-effort: a failure
/// is logged and registration proceeds (PlugRegistry will still load it, just
/// `RTLD_LOCAL`). See [`register_from`] for why this matters.
#[cfg(all(unix, not(any(target_os = "android", target_os = "visionos"))))]
fn preload_plugin_global(dir: &std::path::Path) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    // First promote the already-loaded monolithic `usd_ms` into the GLOBAL scope
    // (RTLD_NOLOAD finds it without loading a second copy; RTLD_GLOBAL promotion is
    // sticky). If the runtime cdylib was itself loaded RTLD_LOCAL (so its usd_ms
    // DT_NEEDED is private), this is what lets the plugin's type_info references
    // resolve to usd_ms's copy below. Best-effort — a NULL return just means it
    // was not found by that soname, which is harmless.
    for soname in ["libusd_ms.so", "libusd_ms.dylib"] {
        let Ok(c) = CString::new(soname) else { continue };
        // SAFETY: a valid NUL-terminated soname; no handle is retained.
        let h = unsafe {
            libc::dlopen(c.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL | libc::RTLD_NOLOAD)
        };
        if !h.is_null() {
            tracing::info!(soname, "promoted usd_ms to RTLD_GLOBAL");
        }
    }

    // Load the plugin into the GLOBAL scope so its weak std::string/VtValue type
    // symbols collapse onto usd_ms's. The library name matters as much as the
    // flag on Android: a bare soname resolves against the app's native-lib dir
    // (the APK `lib/<abi>/`, where the packager stages it), joining the app linker
    // namespace / global group — whereas a data-dir *path* would load outside it
    // and the types would NOT unify (the on-device crash). So try the soname
    // FIRST (the standalone layout), then fall back to the staged-dir path (the
    // desktop-unix layout, where the `.so` sits beside the plugInfo).
    for name in ["libidtx_usd_exec.so", "libidtx_usd_exec.dylib"] {
        let staged = dir.join(name);
        // Prefer: bare soname (app native-lib dir). Fallback: the staged path.
        let candidates: [Option<CString>; 2] = [
            CString::new(name.as_bytes()).ok(),
            staged
                .is_file()
                .then(|| CString::new(staged.as_os_str().as_bytes()).ok())
                .flatten(),
        ];
        for c_path in candidates.into_iter().flatten() {
            // SAFETY: a valid NUL-terminated library name/path; the handle is
            // leaked (never `dlclose`d) so the mapping persists for the process.
            let handle =
                unsafe { libc::dlopen(c_path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
            if !handle.is_null() {
                tracing::info!(
                    lib = %c_path.to_string_lossy(),
                    "loaded OpenExec plugin into RTLD_GLOBAL (app-namespace) scope"
                );
                return;
            }
        }
        tracing::warn!(
            name,
            "RTLD_GLOBAL load of the OpenExec plugin failed (soname + staged path); \
             trying the next platform filename"
        );
        // Try the next platform filename (e.g. `.dylib` on macOS) rather than
        // giving up after the `.so` attempt — otherwise macOS never reaches the
        // dylib and PlugRegistry loads it RTLD_LOCAL, defeating type unification.
        continue;
    }
    tracing::warn!(
        "RTLD_GLOBAL load of the OpenExec plugin failed for every platform filename; \
         cross-library type_info may not unify (compute could fatal on string values)"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn method_and_value_type_tokens_match_schema() {
        assert_eq!(METHOD_TOKENS, ["GET", "POST"]);
        assert_eq!(JSON_VALUE_TYPE_TOKENS, ["none", "float", "double", "string"]);
    }

    #[test]
    fn compute_schema_types_are_a_subset_of_all_types() {
        for t in COMPUTE_SCHEMA_TYPES {
            assert!(
                ALL_SCHEMA_TYPES.contains(&t),
                "{t} missing from ALL_SCHEMA_TYPES"
            );
        }
    }

    /// The baked plugin dir must contain the three artifacts PlugRegistry needs:
    /// the plugin library, `plugInfo.json`, and `generatedSchema.usda`. Guards
    /// against build.rs staging regressions.
    #[test]
    fn baked_plugin_dir_has_library_and_resources() {
        let dir = PathBuf::from(BAKED_PLUGIN_DIR);
        assert!(
            dir.join("plugInfo.json").exists(),
            "plugInfo.json missing in baked dir {}",
            dir.display()
        );
        assert!(
            dir.join("generatedSchema.usda").exists(),
            "generatedSchema.usda missing in baked dir"
        );
        let has_lib = ["idtx_usd_exec.dll", "libidtx_usd_exec.dylib", "libidtx_usd_exec.so"]
            .iter()
            .any(|n| dir.join(n).exists());
        assert!(has_lib, "plugin library missing in baked dir {}", dir.display());
    }

    /// `plugInfo.json` must be a coded (`library`), declare all schema types, and
    /// carry the `Exec > Schemas` discovery block for every compute type.
    ///
    /// Asserted on the raw text (not via `serde_json`): `OpenUSD`'s plugInfo JSON
    /// dialect permits trailing commas, which a strict JSON parser rejects.
    #[test]
    fn plug_info_is_coded_and_declares_exec_schemas() {
        let text = std::fs::read_to_string(PathBuf::from(BAKED_PLUGIN_DIR).join("plugInfo.json"))
            .expect("read plugInfo.json");
        assert!(
            text.contains("\"Type\": \"library\""),
            "coded schema => library plugin"
        );
        // No leading `#` comment lines survive staging (strict-JSON consumers).
        assert!(
            !text.lines().any(|l| l.trim_start().starts_with('#')),
            "staged plugInfo.json must not contain # comment lines"
        );
        for t in ALL_SCHEMA_TYPES {
            assert!(text.contains(&format!("\"IDTX{t}\"")), "plugInfo missing IDTX{t}");
        }
        // The Exec discovery block lists every compute schema.
        assert!(text.contains("\"Exec\""), "plugInfo missing Exec block");
        for t in COMPUTE_SCHEMA_TYPES {
            assert!(
                text.contains(&format!("\"IDTX{t}\": {{}}")),
                "Exec/Schemas missing IDTX{t}"
            );
        }
    }
}
