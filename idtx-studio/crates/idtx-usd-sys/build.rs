//! Build script for `idtx-usd-sys`: links the monolithic `OpenUSD` (`usd_ms`)
//! library and compiles the `cxx` bridge.
//!
//! Flow: resolve the `OpenUSD` install prefix for `CARGO_CFG_TARGET_OS`
//! ([`resolve_usd_dir`]), verify it ([`verify_install`]), prime the MSVC
//! environment on `windows` targets, compile `cxx/usd_shim.cpp` (plus the
//! `OpenExec` and `MaterialX`-WGSL translation units when their features are
//! enabled) into the `idtx_usd_shim` static lib, emit link-search / link-lib /
//! rpath directives (adding sibling oneTBB + `MaterialX` prefixes on Android
//! and visionOS), and write `$OUT_DIR/layout.rs` with the resolved install
//! paths ([`write_layout`]).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR");
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR_ANDROID");
    println!("cargo:rerun-if-env-changed=IDTX_USD_DIR_VISIONOS");
    for path in [
        "build_support/usd_runtime.rs",
        "build.rs",
        "src/bridge.rs",
        "include/usd_shim.h",
        "include/sync_attribute_codec.h",
        "include/materialx_wgsl.h",
        "include/exec_compute.h",
        "cxx/usd_shim.cpp",
        "cxx/usd_exec_bridge.cpp",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest directory"));
    // In a build script `cfg!(target_os = …)` is the HOST, not the compilation
    // target — a landmine when cross-compiling (e.g. a Windows host building for
    // `aarch64-linux-android`). The real target OS is in `CARGO_CFG_TARGET_OS`;
    // every host-vs-target decision below keys off it.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    let install = resolve_usd_dir(&manifest, &target_os);
    verify_install(&install, &target_os);
    let include = install.join("include");
    let lib = install.join("lib");

    // Priming the MSVC environment is only correct when we are actually emitting
    // MSVC objects — a `windows` *target*. On a Windows host cross-compiling to
    // Android the cxx bridge is built by the NDK clang (via `CXX_<triple>`), so
    // priming MSVC here would be wrong.
    if target_os == "windows" {
        idtx_usd_build_support::prime_msvc_env();
    }

    let mut bridge = cxx_build::bridge("src/bridge.rs");
    bridge
        .file("cxx/usd_shim.cpp")
        .include("include")
        .include(&include)
        .std("c++17")
        .warnings(false)
        .flag_if_supported("-Wno-deprecated-declarations");

    let want_materialx = env::var_os("CARGO_FEATURE_MATERIALX_WGSL").is_some();
    // visionOS / Android: the cross-built MaterialX prefix ships only the core
    // library (`libMaterialX`), not the GLSL/HW/Shader generation libs the C++
    // `WgslShaderGenerator` links. The `materialx-wgsl` feature is pulled in
    // transitively by `idtx-render`'s default features even on these targets, so
    // gate the *actual* C++ WGSL path (the define + the generation-lib linking)
    // off on both. `materialx_wgsl.cpp` then compiles to a stub, `idtx-usd`
    // reports the WGSL generator unavailable, and the renderer falls back to its
    // PBR uber-shader — exactly its documented behavior "when the C++
    // WgslShaderGenerator is not compiled in".
    let materialx_wgsl = want_materialx && target_os != "visionos" && target_os != "android";

    // Android/visionOS: oneTBB and MaterialX live in SEPARATE sibling prefixes. A
    // desktop `build_usd.py` install bundles them inside the USD prefix's own
    // `include/` and `lib/`, so `install/{include,lib}` alone is enough. The
    // `xtask build-usd-android` / `build-usd-visionos` cross-builds instead install
    // them under `thirdparty/onetbb-<plat>` and `thirdparty/materialx-<plat>`,
    // leaving the USD prefix with only `pxr/` headers and the monolithic lib. Those
    // sibling prefixes are resolved ONCE here and used for BOTH the compile include
    // path (below) and the link search path (further down) — otherwise core `pxr`
    // headers (`tf/diagnosticMgr.h` → `<tbb/...>`) fail to compile and the linker
    // cannot find the MaterialX generation libs or resolve the monolithic lib's
    // oneTBB dependency.
    let sibling_prefixes = match target_os.as_str() {
        "android" => idtx_usd_build_support::standalone_sibling_prefixes(
            &install,
            &manifest,
            materialx_wgsl,
            ("IDTX_TBB_DIR_ANDROID", "onetbb-android"),
            ("IDTX_MATERIALX_DIR_ANDROID", "materialx-android"),
        ),
        "visionos" => idtx_usd_build_support::standalone_sibling_prefixes(
            &install,
            &manifest,
            materialx_wgsl,
            ("IDTX_TBB_DIR_VISIONOS", "onetbb-visionos"),
            ("IDTX_MATERIALX_DIR_VISIONOS", "materialx-visionos"),
        ),
        _ => Vec::new(),
    };
    for prefix in &sibling_prefixes {
        bridge.include(prefix.join("include"));
    }

    if env::var_os("CARGO_FEATURE_AUTHORING").is_some() {
        bridge.define("IDTX_AUTHORING", None);
    }

    if env::var_os("CARGO_FEATURE_TEST_SUPPORT").is_some() {
        bridge.define("IDTX_TEST_SUPPORT", None);
    }
    // OpenExec dataflow bridge: an extra TU + define. Compiled into the same cxx
    // static lib (links the OpenExec symbols from usd_ms, already linked below),
    // so it needs no separate link lib. Gated so non-exec builds don't pay for it.
    if env::var_os("CARGO_FEATURE_OPENEXEC").is_some() {
        bridge.file("cxx/usd_exec_bridge.cpp");
        bridge.define("IDTX_OPENEXEC", None);
    }
    if materialx_wgsl {
        bridge.define("IDTX_HAVE_MATERIALX_WGSL", None);
    }
    bridge.compile("idtx_usd_shim");

    println!("cargo:rustc-link-search=native={}", lib.display());
    println!("cargo:rustc-link-lib=dylib=usd_ms");
    if materialx_wgsl {
        println!("cargo:rustc-link-lib=dylib=MaterialXGenGlsl");
        println!("cargo:rustc-link-lib=dylib=MaterialXGenHw");
        println!("cargo:rustc-link-lib=dylib=MaterialXGenShader");
        println!("cargo:rustc-link-lib=dylib=MaterialXFormat");
        println!("cargo:rustc-link-lib=dylib=MaterialXCore");
    }
    // Android/visionOS: add the sibling prefixes' `lib/` dirs to the link search so
    // the linker finds the MaterialX generation libs and resolves the monolithic
    // lib's oneTBB `DT_NEEDED`/dependency; link oneTBB explicitly (the shim pulls
    // TBB symbols via inline `pxr` headers). On desktop these libs live in the USD
    // prefix's own `lib/`, already added above.
    for prefix in &sibling_prefixes {
        println!(
            "cargo:rustc-link-search=native={}",
            prefix.join("lib").display()
        );
    }
    if target_os == "android" || target_os == "visionos" {
        println!("cargo:rustc-link-lib=dylib=tbb");
    }
    match target_os.as_str() {
        "macos" => {
            // cargo:rustc-link-arg passes directly to ld64 — two separate args.
            println!("cargo:rustc-link-arg=-rpath");
            println!("cargo:rustc-link-arg={}", lib.display());
        }
        "linux" => {
            println!("cargo:rustc-link-arg=-Wl,-rpath,{}", lib.display());
        }
        // Android / visionOS (and any other target): NO rpath. On-device the native
        // libraries are loaded from the package's own lib dir (the APK's
        // `lib/<abi>/`, the `.app`'s `Frameworks/`) by the system linker; a
        // build-host rpath would be meaningless (and the install prefix does not
        // exist on-device).
        _ => {}
    }
    println!("cargo:USD_DIR={}", install.display());
    write_layout(&install, &lib);
}

/// Resolve the `OpenUSD` install prefix for the compilation `target_os`.
///
/// Thin wrapper over the shared
/// [`idtx_usd_build_support::resolve_usd_dir_for_target`] — the single source of
/// truth for USD-prefix resolution shared across the workspace's build scripts.
fn resolve_usd_dir(manifest_dir: &Path, target_os: &str) -> PathBuf {
    idtx_usd_build_support::resolve_usd_dir_for_target(manifest_dir, target_os)
}

/// Assert the resolved `install` prefix is a usable `OpenUSD` build for `target_os`.
///
/// # Panics
///
/// Panics if `include/pxr/pxr.h` cannot be read, if it does not define
/// `PXR_VERSION 2611` (`OpenUSD` 26.11), or if the platform-appropriate
/// monolithic `usd_ms` library is missing from `lib/` (`.lib` on Windows,
/// `.dylib` on Apple targets, `.so` on Linux/Android).
fn verify_install(install: &Path, target_os: &str) {
    let header = install.join("include/pxr/pxr.h");
    let text = fs::read_to_string(&header)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", header.display()));
    assert!(
        text.contains("#define PXR_VERSION 2611"),
        "OpenUSD 26.11 required"
    );
    let library = match target_os {
        // Apple targets (desktop macOS + visionOS) ship the Mach-O `.dylib`.
        "macos" | "visionos" => "libusd_ms.dylib",
        "windows" => "usd_ms.lib",
        // linux + android share the ELF `.so` name.
        _ => "libusd_ms.so",
    };
    assert!(
        install.join("lib").join(library).is_file(),
        "missing usd_ms"
    );
}

/// Write `$OUT_DIR/layout.rs` exposing the resolved `OpenUSD` paths as
/// `pub const` string literals for the crate to `include!` at compile time:
/// `USD_DIR` (install prefix), `USD_LIB_DIR` (`lib/`), `USD_PLUGIN_DIR`
/// (`<install>/plugin`), and `USD_RESOURCE_DIR` (`<lib>/usd`).
///
/// # Panics
///
/// Panics if `OUT_DIR` is unset (Cargo always sets it for a build script) or
/// if the layout module cannot be written.
fn write_layout(install: &Path, lib: &Path) {
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("layout.rs");
    let text = format!(
        "pub const USD_DIR: &str = {install:?};\n\
         pub const USD_LIB_DIR: &str = {lib:?};\n\
         pub const USD_PLUGIN_DIR: &str = {plugin:?};\n\
         pub const USD_RESOURCE_DIR: &str = {resource:?};\n",
        install = install.display().to_string(),
        lib = lib.display().to_string(),
        plugin = install.join("plugin").display().to_string(),
        resource = lib.join("usd").display().to_string(),
    );
    fs::write(output, text).expect("write OpenUSD layout module");
}
