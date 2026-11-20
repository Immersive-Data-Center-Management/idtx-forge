//! Build script for the `idtx-runtime` player binary / Android+visionOS cdylib.
//!
//! Two jobs:
//!  1. Stage `OpenUSD`'s `usd_ms` dynamic library + resource trees next to the
//!     produced binary so it can open stages. The single source of truth lives in
//!     `crates/idtx-usd-sys/build_support/usd_runtime.rs`; every binary crate's
//!     `build.rs` `#[path]`-includes it and calls `stage_usd_runtime()`.
//!  2. On standalone targets (Android / visionOS), force a `DT_NEEDED` /
//!     `LC_LOAD_DYLIB` on each configured USD **extension** plugin so the dynamic
//!     linker loads it in this runtime's dependency closure — the same linker
//!     namespace as `usd_ms`. That is what makes the plugin's and the core's weak
//!     C++ `type_info` / `VtValue` symbols collapse to one copy; otherwise (a late
//!     `dlopen`) they split on bionic / Apple libc++ and `OpenExec` aborts ("Vector
//!     is holding string, tried to use as string"). Extensions need NO cooperation
//!     (no anchor symbol): the runtime — the crate that actually produces the
//!     cdylib — drives the link here, because `--no-as-needed` link-args do not
//!     propagate from a dependency's build script and Rust's default `--as-needed`
//!     would drop an unreferenced plugin dependency.
//!
//! This is the "Option C" extension-loading model: an extension ships a plugin
//! `.so` (staged into the APK native-lib dir / `.app` Frameworks by the packager)
//! and declares it so the runtime cross-build links it; nothing is injected into a
//! finished binary. Desktop keeps the `PlugRegistry` dlopen model (Windows/Linux
//! compare `type_info` by name, so the split does not occur there).

#[path = "../../crates/idtx-usd-sys/build_support/usd_runtime.rs"]
mod usd_runtime;

use std::path::Path;

fn main() {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest.parent().unwrap().parent().unwrap();
    idtx_build_metadata::emit_runtime_metadata(workspace, manifest, idtx_behavior_abi::ABI_VERSION)
        .expect("capture native runtime build metadata");
    usd_runtime::stage_usd_runtime();
    usd_runtime::bake_http_resolver_rpath();
    link_standalone_usd_extensions();
}

/// Emit `DT_NEEDED` link directives for every configured standalone USD extension.
/// No-op on desktop (the `[[bin]]` link + the PlugRegistry dlopen model is used
/// there). Each extension exports its staged plugin dir via its crate `links` key
/// (`DEP_<LINKS>_PLUGIN_DIR`); add a block per extension below as more are wired.
fn link_standalone_usd_extensions() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "android" && target_os != "visionos" {
        return;
    }

    // Built-in coded OpenExec plugin (feature `openexec`). Its build script
    // exported `DEP_IDTX_USD_EXEC_PLUGIN_DIR` (the staged `lib`/dir holding
    // `libidtx_usd_exec.{so,dylib}`).
    if std::env::var_os("CARGO_FEATURE_OPENEXEC").is_some() {
        println!("cargo:rerun-if-env-changed=DEP_IDTX_USD_EXEC_PLUGIN_DIR");
        if let Some(dir) = std::env::var_os("DEP_IDTX_USD_EXEC_PLUGIN_DIR") {
            emit_cdylib_dt_needed(Path::new(&dir), "idtx_usd_exec", &target_os);
        }
    }
}

/// Force the cdylib to take a `DT_NEEDED` / `LC_LOAD_DYLIB` on `lib<lib_stem>` in
/// `plugin_dir`, without referencing any of its symbols.
///
/// `rustc-link-arg-cdylib` applies to the cdylib produced by THIS crate (the
/// runtime), so the args land on the final link where `--no-as-needed` can bracket
/// the `-l` and defeat Rust's default `--as-needed`. On ld64 (Apple) every linked
/// dylib is recorded, so a plain `-l` suffices.
fn emit_cdylib_dt_needed(plugin_dir: &Path, lib_stem: &str, target_os: &str) {
    let dir = plugin_dir.display();
    match target_os {
        "android" => {
            println!("cargo:rustc-link-arg-cdylib=-L{dir}");
            println!("cargo:rustc-link-arg-cdylib=-Wl,--no-as-needed");
            println!("cargo:rustc-link-arg-cdylib=-Wl,-l{lib_stem}");
            println!("cargo:rustc-link-arg-cdylib=-Wl,--as-needed");
        }
        "visionos" => {
            println!("cargo:rustc-link-arg-cdylib=-L{dir}");
            println!("cargo:rustc-link-arg-cdylib=-l{lib_stem}");
        }
        _ => {}
    }
}
