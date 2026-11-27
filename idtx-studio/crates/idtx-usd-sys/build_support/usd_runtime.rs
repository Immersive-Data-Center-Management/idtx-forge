//! Shared USD-runtime staging logic for the three shell "flavor" binaries
//! (`idtx-editor`, `idtx-inspector`, `idtx-builder`).
//!
//! # Why this file exists
//!
//! All three binaries are thin wrappers over `idtx-studio-shell` gated by a
//! runtime `ProductProfile`. They each need the same post-build side effect:
//! make `OpenUSD`'s monolithic `usd_ms` dynamic library (plus its TBB and
//! `MaterialX` dependencies as well as the `usd/` + `plugin/` resource
//! trees) discoverable next to the produced binary so that
//! `cargo run -p idtx-{editor, inspector, builder}` Just Works™ without the
//! user having to set `DYLD_LIBRARY_PATH` / `LD_LIBRARY_PATH` or hand-copy
//! anything.
//!
//! # Why it can't live in a normal library module
//!
//! Cargo's `cargo:rustc-link-arg-bins=` directive only affects bin targets
//! in the *same package* whose build script emitted it — link args do not
//! propagate transitively to downstream binary crates. Likewise the
//! same-directory dylib copy is keyed off `OUT_DIR`, which is per-crate.
//! So the *logic* is shared here as a single source of truth, but it must
//! be **executed from each binary crate's own `build.rs`**. Each of the
//! three `build.rs` files does nothing but `#[path]`-include this file and
//! call [`stage_usd_runtime`]. The code is written exactly once.
//!
//! # `OpenUSD` Install Resolution rules
//!
//! Mirrors `crates/idtx-usd-sys/build.rs`:
//!   1. `$IDTX_USD_DIR` if set.
//!   2. Otherwise `<workspace>/third_party/openusd/install`.
//!
//! If the monolithic lib is not present under `<usd_dir>/lib` we create an
//! error message and the build fails

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Entry point invoked from each flavor binary's `build.rs`.
///
/// On Unix (macOS / Linux) it emits an rpath scoped to this package's bin
/// targets so the dynamic loader finds `@rpath/libusd_ms.dylib` at run
/// time. On Windows — which has no rpath and resolves DLLs via the
/// executable's directory — it copies `usd_ms.dll` + TBB DLLs next to the
/// binary. In every case it also stages the `usd/` resource tree and the
/// `plugin/` tree next to the binary so USD's plugin discovery works from
/// a plain `cargo run`.
///
/// # Panics
///
/// Panics if `OUT_DIR` is not set. Cargo always sets it when running a build
/// script, so this cannot occur in normal use.
pub fn stage_usd_runtime() {
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    // Stage the workspace `assets/` tree (default controller meshes) next to the
    // binary first — this is independent of the OpenUSD install, so it must run
    // before the early returns below (a `cargo check`-only profile with no USD
    // dir should still get its assets staged).
    stage_default_assets();

    let Some(usd_dir) = resolve_usd_dir() else {
        return;
    };
    let lib_dir = usd_dir.join("lib");
    let bin_dir = usd_dir.join("bin");
    if !monolithic_lib_present(&lib_dir) {
        // The USD install either doesn't exist or is incomplete. If the
        // user is trying to link `openusd`, `idtx-usd-sys` will
        // already print a much better diagnostic — so we stay quiet here
        // beyond a single hint to avoid duplicating it.
        println!(
            "cargo:warning=idtx-studio-shell: could not find monolithic usd lib under {}",
            lib_dir.display()
        );
        return;
    }

    // On Unix an rpath is enough: the upstream OpenUSD build stamps
    // `libusd_ms.dylib` with install_name `@rpath/libusd_ms.dylib`, so
    // pinning an rpath to the resolved lib dir lets the loader find it
    // without copying. `rustc-link-arg-bins` scopes the arg to bin targets
    // in *this* package (no effect on unit tests or dependents).
    if cfg!(any(target_os = "macos", target_os = "linux")) {
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{}", lib_dir.display());
        // Fall through: we still stage the usd/ + plugin/ resource trees
        // below so plugin discovery works from `cargo run`.
    }

    // Compute <target>/<profile> from OUT_DIR
    // (`target/<profile>/build/<crate-hash>/out`).
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let Some(profile_dir) = out_dir
        .ancestors()
        .nth(3) // out -> <crate-hash> -> build -> <profile>
        .map(PathBuf::from)
    else {
        println!("cargo:warning=idtx-shell: could not derive profile dir from OUT_DIR");
        return;
    };

    // Windows has no rpath and resolves DLLs from the executable's
    // directory — so the dylibs must be copied next to the binary.
    if cfg!(target_os = "windows") {
        copy_glob(&lib_dir, &profile_dir, &["usd_ms.dll"]);
        copy_glob(
            &bin_dir,
            &profile_dir,
            &[
                "usd_ms.dll",
                "MaterialXRenderOsl.dll",
                "MaterialXCore.dll",
                "MaterialXFormat.dll",
                "MaterialXGenGlsl.dll",
                "MaterialXGenHw.dll",
                "MaterialXGenMdl.dll",
                "MaterialXGenMsl.dll",
                "MaterialXGenOsl.dll",
                "MaterialXGenShader.dll",
                "MaterialXGenSlang.dll",
                "MaterialXRender.dll",
                "MaterialXRenderGlsl.dll",
                "MaterialXRenderHw.dll",
            ],
        );
        let tbb = &[
            "tbb12.dll",
            "tbb.dll",
            "tbbmalloc.dll",
            "tbbmalloc_proxy.dll",
        ];
        copy_glob(&bin_dir, &profile_dir, tbb);
        copy_glob(&lib_dir, &profile_dir, tbb);
    }

    // Stage the `usd/` resource directory next to the executable so USD's
    // plugin discovery (`PXR_PLUGINPATH_NAME`) resolves against a local
    // tree. Harmless on Unix too.
    if lib_dir.join("usd").is_dir() {
        let dst = profile_dir.join("usd");
        let _ = fs::create_dir_all(&dst);
        copy_dir_recursive(&lib_dir.join("usd"), &dst);
    }

    // Stage the top-level `plugin/` directory next to the executable.
    let plugin_dir = usd_dir.join("plugin");
    if plugin_dir.is_dir() {
        let dst = profile_dir.join("plugin");
        let _ = fs::create_dir_all(&dst);
        copy_dir_recursive(&plugin_dir, &dst);
    }
}

/// Bake an rpath to the HTTP-resolver plugin's build `OUT_DIR` into this crate's
/// **binary** targets, so a dev `cargo run` / editor-spawned preview satisfies the
/// forced `LC_LOAD_DYLIB @rpath/libidtx_http_resolver.dylib` (the `remote` feature
/// links the resolver as a hard dependency) without `DYLD_*` env or a cooked
/// bundle — the loader resolves that load **before `main`**.
///
/// Must run from the binary crate's own `build.rs`, for the same reason as
/// [`stage_usd_runtime`]: `rustc-link-arg-bins` is per-package and does not
/// propagate to downstream binaries. The resolver crate (a library) therefore
/// *cannot* bake this into a dependent bin — its own `rustc-link-arg` scopes only
/// its own test/example binaries — so it publishes its plugin dir via its `links`
/// key and the bin crate reads it here as `DEP_IDTX_HTTP_RESOLVER_PLUGIN_DIR`.
///
/// Host macOS/Linux only, keyed off `CARGO_CFG_TARGET_OS` (the *real* build
/// target — a build script's `cfg!(target_os)` is the HOST, a cross-compile
/// landmine). Android / visionOS load the staged plugin from the package's lib dir
/// / `.app` `Frameworks/` instead; and `cargo xtask bundle-mac --remote` deletes
/// this absolute rpath from the shipped binary and swaps in the bundle-relative
/// `@executable_path/../Frameworks/idtx-http-resolver` (`bundle_resolver_plugin`).
///
/// A no-op unless the `remote` feature is on AND the resolver is a *direct*
/// dependency of the calling crate (only then does Cargo set the `DEP_…` var).
#[allow(dead_code)] // included via `#[path]` by binary crates that do not all call it
pub fn bake_http_resolver_rpath() {
    if env::var_os("CARGO_FEATURE_REMOTE").is_none() {
        return;
    }
    println!("cargo:rerun-if-env-changed=DEP_IDTX_HTTP_RESOLVER_PLUGIN_DIR");
    let Some(plugin_dir) = env::var_os("DEP_IDTX_HTTP_RESOLVER_PLUGIN_DIR") else {
        return;
    };
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os == "macos" || target_os == "linux" {
        println!(
            "cargo:rustc-link-arg-bins=-Wl,-rpath,{}",
            Path::new(&plugin_dir).display()
        );
    }
}

/// Stage the workspace `assets/` tree (default controller meshes and any other
/// shipped runtime assets) next to the produced binary, so the runtime manifest
/// resolves `assets/controllers/...` against its `<exe>/assets` search root with
/// no manual copy. Mirrors the USD resource staging: profile dir from `OUT_DIR`,
/// workspace root from `CARGO_MANIFEST_DIR`. Independent of the `OpenUSD` install;
/// a missing `assets/` dir is a silent no-op.
fn stage_default_assets() {
    let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    // CARGO_MANIFEST_DIR = <workspace>/{apps,crates}/<name>; up two = workspace.
    let Some(workspace_root) = Path::new(&manifest_dir).parent().and_then(Path::parent) else {
        return;
    };
    let assets_src = workspace_root.join("assets");
    if !assets_src.is_dir() {
        return;
    }
    println!("cargo:rerun-if-changed={}", assets_src.display());

    // <target>/<profile> from OUT_DIR (`target/<profile>/build/<hash>/out`).
    let Some(out_dir) = env::var_os("OUT_DIR") else {
        return;
    };
    let Some(profile_dir) = PathBuf::from(out_dir).ancestors().nth(3).map(PathBuf::from) else {
        return;
    };
    let dst = profile_dir.join("assets");
    let _ = fs::create_dir_all(&dst);
    copy_dir_recursive(&assets_src, &dst);
}

fn resolve_usd_dir() -> Option<PathBuf> {
    if let Some(dir) = env::var_os("IDTX_USD_DIR") {
        return Some(PathBuf::from(dir));
    }
    // CARGO_MANIFEST_DIR = <workspace>/crates/idtx-<flavor>
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").ok()?);
    let workspace_root = manifest_dir.parent()?.parent()?.to_path_buf();
    Some(
        workspace_root
            .join("third_party")
            .join("openusd")
            .join("install"),
    )
}

fn monolithic_lib_present(lib_dir: &Path) -> bool {
    let candidates: &[&str] = if cfg!(target_os = "windows") {
        &["usd_ms.lib"]
    } else if cfg!(target_os = "macos") {
        &["libusd_ms.dylib"]
    } else {
        &["libusd_ms.so"]
    };
    candidates.iter().any(|n| lib_dir.join(n).exists())
}

fn copy_glob(src_dir: &Path, dst_dir: &Path, names: &[&str]) {
    if !src_dir.is_dir() {
        return;
    }
    for name in names {
        let s = src_dir.join(name);
        if s.is_file() {
            let d = dst_dir.join(name);
            if let Err(e) = fs::copy(&s, &d) {
                println!(
                    "cargo:warning=idtx-shell: failed to copy {} -> {}: {e}",
                    s.display(),
                    d.display()
                );
            }
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    if !src.is_dir() {
        return;
    }
    let Ok(read) = fs::read_dir(src) else { return };
    for entry in read.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let target = dst.join(&name);
        if path.is_dir() {
            let _ = fs::create_dir_all(&target);
            copy_dir_recursive(&path, &target);
        } else if path.is_file() {
            let _ = fs::copy(&path, &target);
        }
    }
}
